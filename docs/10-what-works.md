# What works, and what does not

This document holds the state of the filesystem. A limit here is a limit the
project knows about, and each one has a reason.

## What works

| Operation                  | Command        | State |
| -------------------------- | -------------- | ----- |
| Mount                      | `mtpfs`        | yes   |
| Stop the mount             | `umount`       | yes   |
| List a folder              | `ls`           | yes   |
| Read the size and the kind | `stat`         | yes   |
| Read a file                | `cp`, `cat`    | yes   |
| Read part of a file        | `head`, `dd`   | yes   |
| Write a new file           | `cp`           | yes   |
| Copy with the attributes   | `cp -p`        | yes   |
| Make a folder              | `mkdir`        | yes   |
| Remove a file              | `rm`           | yes   |
| Remove a folder            | `rmdir`        | yes   |
| Give a file a new name     | `mv`           | yes   |
| Move a file to a folder    | `mv`           | yes   |
| Report the free space      | `df`           | yes   |
| Search a tree              | `find`         | yes   |

A read gives the same bytes as a copy over `adb`. A test compares a SHA-256 sum
for a file of 290 KB, and for a file of 450 MB.

## What does not work

### A change to a file that is on the device

A program that opens a file on the device and writes to the file gets a fault.

MTP holds two operations for this, and both test devices support them. The
project does not use them yet. A new file works, and a change to an old file
does not.

The safe way today needs three steps:

1. Copy the file to the disk of the host.
2. Change the file on the host.
3. Copy the file back to the device, with a new name.

### A time, a mode and an owner

MTP holds none of the three. The filesystem accepts each request and changes
nothing, so `cp -p` works and reports no fault.

A time in a listing is therefore not the time of the file. `ls -l` shows
`Dec 31 1969` for each object.

### A link

MTP holds no symbolic link and no hard link. The filesystem gives a fault for
each one.

### A name that two objects share

MTP allows two objects in one folder with the same name. A filesystem does not.

The filesystem shows both objects in a listing, and a read of the name gives
the first object. The second object is then not reachable by name.

No test device holds such a pair. The `tree` module holds a test for the
behaviour, so a later version has a place to start.

### A name with a separator

A name from a device can hold `/`, which a path separator cannot. The
filesystem replaces each `/` with `_` in a listing.

## What is slow, and why

### One operation at a time

MTP holds one session, and a session does one operation at a time. The mount
runs in one thread for that reason.

A program that reads two files at once does not go faster. The two reads take
turns.

### The cache holds one part of one file

The host reads 4 MB in advance, and keeps the rest for the next read. A copy
reads a file from the start to the end, so the next read almost always follows
the last one.

The cache holds one range, for one object. A program that reads two files at
the same time loses the cache at each change. Each read then costs a request to
the device.

A copy of one file after another is therefore fast. A copy of two files at the
same time is slow.

### A listing costs one request for each object

`GetObjectHandles` gives the handles of a folder in one request. The name and
the size of each object then cost one request each.

A folder with 500 objects costs 501 requests. At about 20 milliseconds each,
the listing takes about 10 seconds.

A device gives an operation that reads a folder in one request,
`GetObjectPropList`, and both test devices support the operation. The project
does not use the operation yet. This is the largest improvement that remains.

### A listing does not change

The filesystem reads a folder one time, and keeps the answer while the mount
runs.

A file that another program adds to the device does not appear. A write through
the mount does update the listing of that folder.

Stop the mount and start the mount again to see a change from another program.

## What costs memory

A program closes a new file, and the file then goes to the device. MTP needs
the size of a file before the bytes. A program gives no size in advance.

The host therefore holds the whole file in memory until the close. A copy of a
file of 4 GB to the device needs 4 GB of memory.

A read costs 4 MB, and not the size of the file.

## The device, and not the filesystem

| Limit                        | Cause                                     |
| ---------------------------- | ----------------------------------------- |
| One storage                  | The filesystem uses the first storage of the device |
| A rate of about 30 MB/s      | The USB link. See `docs/04-file-transfer.md` |
| No file while the screen locks | The device stops the session            |

## A device this project does not find

The filesystem reads the USB descriptor, and finds two shapes of MTP
interface. A third shape gives no mount.

`libmtp` reads a Microsoft descriptor at string index 0xee, and this project
does not. A device with a vendor class, and an interface name that is not
`MTP`, needs that test.

See `docs/05-finding-the-interface.md`, and send a report:

```
BSDROID_DEBUG=1 mtpprobe probe
```
