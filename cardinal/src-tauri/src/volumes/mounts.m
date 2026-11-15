#import <AppKit/AppKit.h>
#import <CoreServices/CoreServices.h>
#import <DiskArbitration/DiskArbitration.h>
#include <sys/mount.h>
#include <sys/stat.h>

// NSWorkspace delivers hot-plug and rename notifications on the app run loop.
void cardinal_watch_mounts(void (*changed)(void)) {
    NSNotificationCenter *center = NSWorkspace.sharedWorkspace.notificationCenter;
    for (NSString *name in @[NSWorkspaceDidMountNotification, NSWorkspaceDidUnmountNotification,
                             NSWorkspaceDidRenameVolumeNotification]) {
        [center addObserverForName:name object:nil queue:nil usingBlock:^(__unused NSNotification *note) { changed(); }];
    }
}

typedef void (*MountVisitor)(void *, const char *, const char *, const char *, const char *, int32_t, bool);

void cardinal_mount_paths(void *context, void (*visit)(void *, const char *)) {
    struct statfs *mounts = NULL;
    int count = getmntinfo_r_np(&mounts, MNT_NOWAIT);
    for (int i = 0; i < count; i++) visit(context, mounts[i].f_mntonname);
    free(mounts);
}

// getmntinfo_r_np(NOWAIT) consults the mount table without waking network disks.
// Disk Arbitration distinguishes fixed-in-a-USB-enclosure from internal media.
bool cardinal_visit_mounts(void *context, MountVisitor visit) {
    @autoreleasepool {
        struct statfs *mounts = NULL;
        int count = getmntinfo_r_np(&mounts, MNT_NOWAIT);
        if (count <= 0) return false;
        DASessionRef session = DASessionCreate(NULL);
        if (!session) { free(mounts); return false; }
        for (int i = 0; i < count; i++) {
            struct statfs *m = &mounts[i];
            if (!(m->f_flags & MNT_LOCAL) || strncmp(m->f_mntfromname, "/dev/disk", 9)) continue;
            DADiskRef disk = DADiskCreateFromBSDName(NULL, session, m->f_mntfromname);
            if (!disk) continue;
            NSDictionary *info = CFBridgingRelease(DADiskCopyDescription(disk));
            CFRelease(disk);
            NSNumber *internal = info[(__bridge NSString *)kDADiskDescriptionDeviceInternalKey];
            NSNumber *ejectable = info[(__bridge NSString *)kDADiskDescriptionMediaEjectableKey];
            NSNumber *removable = info[(__bridge NSString *)kDADiskDescriptionMediaRemovableKey];
            if (!internal && !ejectable.boolValue && !removable.boolValue) {
                NSURL *url = [NSURL fileURLWithFileSystemRepresentation:m->f_mntonname isDirectory:YES relativeToURL:nil];
                [url getResourceValue:&internal forKey:NSURLVolumeIsInternalKey error:nil];
                if (!internal) continue;
            }
            if (internal.boolValue) continue;
            if (m->f_flags & MNT_DONTBROWSE && strncmp(m->f_mntonname, "/Volumes/", 9)) continue;
            struct stat st;
            if (stat(m->f_mntonname, &st) != 0) continue;
            CFUUIDRef uuid = (__bridge CFUUIDRef)info[(__bridge NSString *)kDADiskDescriptionVolumeUUIDKey];
            NSString *identity = uuid ? CFBridgingRelease(CFUUIDCreateString(NULL, uuid)) : nil;
            CFUUIDRef journal = FSEventsCopyUUIDForDevice(st.st_dev);
            NSString *epoch = journal ? CFBridgingRelease(CFUUIDCreateString(NULL, journal)) : nil;
            if (journal) CFRelease(journal);
            visit(context, m->f_mntonname, m->f_fstypename, identity.UTF8String, epoch.UTF8String, st.st_dev, (m->f_flags & MNT_RDONLY) != 0);
        }
        free(mounts);
        CFRelease(session);
        return true;
    }
}
