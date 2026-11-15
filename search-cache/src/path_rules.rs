use super::*;
use std::path::Component;

/// Scope a preference to the actual watched tree. The local index also stores
/// ancestors above this root so a result can be expanded into an absolute path.
fn scoped_rule(rule: &Path, watched: &Path) -> Option<PathBuf> {
    if !rule.is_absolute() || rule.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    if rule.starts_with(watched) {
        Some(rule.to_owned())
    } else if watched.starts_with(rule) {
        Some(watched.to_owned())
    } else {
        None
    }
}

fn differences<'a>(left: &'a [PathBuf], right: &'a [PathBuf]) -> impl Iterator<Item = &'a PathBuf> {
    left.iter().filter(move |p| !right.contains(p))
}

fn highest_hidden_parent(
    mut path: PathBuf,
    watched: &Path,
    hidden: impl Fn(&Path) -> bool,
) -> PathBuf {
    while path != watched && path.parent().is_some_and(&hidden) {
        path.pop();
    }
    path
}

impl SearchCache {
    /// Reconcile a complete baseline with new path rules. Narrowing only
    /// touches indexed nodes. A caller with an unfinished expansion passes its
    /// roots back in so superseding preferences cannot omit unseen descendants.
    /// While the returned plan is running, keep the previous durable snapshot.
    pub fn update_path_rules(
        &mut self,
        ignore: &[PathBuf],
        include: &[PathBuf],
        restoring: &mut Vec<PathBuf>,
    ) -> Option<EventPlan> {
        let watched = self.file_nodes.path().to_owned();
        let old_ignore = self.file_nodes.ignore_paths();
        let old_include = self.file_nodes.include_paths();
        let previous = |p: &Path| should_ignore_path(p, old_ignore, old_include);
        let next = |p: &Path| should_ignore_path(p, ignore, include);

        let narrowed: Vec<_> = differences(ignore, old_ignore)
            .chain(differences(old_include, include))
            .filter_map(|rule| scoped_rule(rule, &watched))
            .map(|path| highest_hidden_parent(path, &watched, next))
            .collect();

        restoring.extend(
            differences(old_ignore, ignore)
                .chain(differences(include, old_include))
                .filter_map(|rule| scoped_rule(rule, &watched))
                .filter(|path| !next(path))
                .map(|path| highest_hidden_parent(path, &watched, previous)),
        );
        restoring.retain(|path| !next(path));
        restoring.sort();
        let mut roots: Vec<PathBuf> = Vec::new();
        for path in restoring.drain(..) {
            // Path ordering compares components, so all descendants follow
            // their parent contiguously. Duplicate rules are harmless.
            if roots.last().is_none_or(|root| !path.starts_with(root)) {
                roots.push(path);
            }
        }
        *restoring = roots;
        self.file_nodes.set_paths(ignore, include);
        for path in narrowed {
            self.prune_exclusion(&path, &watched);
        }
        (!restoring.is_empty()).then(|| EventPlan::restore_paths(self, restoring))
    }

    fn prune_exclusion(&mut self, path: &Path, watched: &Path) {
        let Some(id) = self.node_index_for_path(path) else {
            return;
        };
        if self.should_ignore(path) && path != watched {
            self.remove_node(id);
            return;
        }
        let mut pending = vec![(id, path.to_owned())];
        while let Some((parent, path)) = pending.pop() {
            // A retained ancestor may still have excluded siblings along the
            // route to an explicit include. The watched root itself is always
            // present, just as it is in an initial scan.
            if !self.should_ignore(&path)
                && !self
                    .file_nodes
                    .include_paths()
                    .iter()
                    .any(|p| p != &path && p.starts_with(&path))
            {
                continue;
            }
            for child in std::mem::take(&mut self.file_nodes[parent].children) {
                let child_path = path.join(self.file_nodes[child].name());
                if self.should_ignore(&child_path) {
                    self.remove_detached_subtree(child);
                } else {
                    self.file_nodes[parent].children.push(child);
                    pending.push((child, child_path));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};
    use tempdir::TempDir;

    fn apply(plan: Option<EventPlan>, cache: &mut SearchCache) {
        if let Some(plan) = plan {
            plan.read(
                || false,
                |change| {
                    cache.apply_observation(change);
                },
            );
        }
    }

    #[test]
    fn exact_file_and_subtree_exclusions_preserve_neighbors_and_cursor() {
        let tmp = TempDir::new("cardinal-rules").unwrap();
        let root = tmp.path().join("watched");
        let folder = root.join("sample");
        let sibling = root.join("sample-old");
        let spaced = root.join("notes ");
        let plain = root.join("notes");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&sibling).unwrap();
        fs::write(folder.join("child"), b"old").unwrap();
        fs::write(&spaced, b"spaced").unwrap();
        fs::write(&plain, b"plain").unwrap();
        let mut cache = SearchCache::walk_fs(&root);
        let from = cache.last_event_id();
        let neighbor = cache.node_index_for_path(&plain).unwrap();
        let mut pending = Vec::new();
        assert!(
            cache
                .update_path_rules(&[folder.clone(), spaced.clone()], &[], &mut pending)
                .is_none()
        );
        assert!(cache.node_index_for_path(&folder).is_none());
        assert!(cache.node_index_for_path(&folder.join("child")).is_none());
        assert!(cache.node_index_for_path(&spaced).is_none());
        assert!(cache.node_index_for_path(&sibling).is_some());
        assert_eq!(cache.node_index_for_path(&plain), Some(neighbor));
        fs::write(folder.join("new"), b"new").unwrap();
        fs::remove_file(folder.join("child")).unwrap();
        fs::write(sibling.join("event-not-yet-delivered"), b"later").unwrap();
        let plan = cache.update_path_rules(std::slice::from_ref(&spaced), &[], &mut pending);
        assert_eq!(pending, vec![folder.clone()]);
        apply(plan, &mut cache);
        assert!(cache.node_index_for_path(&folder.join("new")).is_some());
        assert!(cache.node_index_for_path(&folder.join("child")).is_none());
        assert!(
            cache
                .node_index_for_path(&sibling.join("event-not-yet-delivered"))
                .is_none()
        );
        assert_eq!(cache.node_index_for_path(&plain), Some(neighbor));
        assert_eq!(cache.last_event_id(), from);
        assert_eq!(cache.rescan_count(), 0);
    }

    #[test]
    fn policy_changes_match_a_fresh_walk_with_nested_include_exceptions() {
        let tmp = TempDir::new("cardinal-rules-includes").unwrap();
        let root = tmp.path().join("watched");
        let tree = root.join("tree");
        let keep = tree.join("a/b");
        let hidden = tree.join("a/hidden");
        let nested = keep.join("more");
        fs::create_dir_all(&nested).unwrap();
        fs::create_dir(&hidden).unwrap();
        fs::write(keep.join("visible"), b"yes").unwrap();
        fs::write(nested.join("file"), b"nested").unwrap();
        fs::write(tree.join("top"), b"sibling").unwrap();
        let mut cache = SearchCache::walk_fs(&root);
        let rules = [
            (vec![tree.clone()], vec![keep.clone()]),
            (vec![tree.clone(), nested.clone()], vec![keep.clone()]),
            (
                vec![tree.clone(), nested.clone()],
                vec![keep.clone(), nested.clone()],
            ),
            (vec![tree.clone(), keep.clone()], vec![]),
            (vec![tree.clone()], vec![nested.clone()]),
            (vec![], vec![]),
            (vec![tmp.path().to_owned()], vec![]),
            (vec![], vec![]),
        ];
        for (ignore, include) in rules {
            let mut pending = vec![];
            let plan = cache.update_path_rules(&ignore, &include, &mut pending);
            apply(plan, &mut cache);
            let expected = SearchCache::walk_fs_with_walk_data(
                &WalkData::new(&root, &ignore, &include, true, || false),
                &NEVER_STOPPED,
            )
            .unwrap();
            let paths = |c: &SearchCache| {
                c.search_empty(CancellationToken::noop())
                    .unwrap()
                    .into_iter()
                    .filter_map(|id| c.node_path(id))
                    .collect::<std::collections::BTreeSet<_>>()
            };
            assert_eq!(
                paths(&cache),
                paths(&expected),
                "ignore {ignore:?}, include {include:?}"
            );
        }
    }

    #[test]
    fn superseded_expansion_resumes_with_the_latest_filters_without_following_symlinks() {
        let tmp = TempDir::new("cardinal-rules-pending").unwrap();
        let root = tmp.path().join("watched");
        let external = tmp.path().join("outside");
        let directory = root.join("tree");
        let blocked = directory.join("secret");
        fs::create_dir_all(&directory).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("private"), b"not indexed").unwrap();
        fs::write(&blocked, b"blocked").unwrap();
        fs::write(directory.join("ok"), b"ok").unwrap();
        let link = root.join("alias");
        symlink(&external, &link).unwrap();
        let linked_child = link.join("private");
        let escaped = root.join("../outside/private");
        let mut cache = SearchCache::walk_fs_with_ignore(
            &root,
            &[directory.clone(), linked_child.clone(), escaped.clone()],
        );
        let mut pending = vec![];
        let from = cache.last_event_id();
        let first = cache.update_path_rules(&[], &[], &mut pending).unwrap();
        first.read(
            || false,
            |change| {
                if matches!(change, EventObservation::Node(..)) {
                    cache.apply_observation(change);
                }
            },
        );
        let next = cache.update_path_rules(std::slice::from_ref(&blocked), &[], &mut pending);
        apply(next, &mut cache);
        assert!(cache.node_index_for_path(&directory.join("ok")).is_some());
        assert!(cache.node_index_for_path(&blocked).is_none());
        assert!(cache.node_index_for_path(&link).is_some());
        assert!(cache.node_index_for_path(&linked_child).is_none());
        assert!(cache.node_index_for_path(&escaped).is_none());
        assert_eq!(from, cache.last_event_id());
    }
}
