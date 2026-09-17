# What works, and what does not

This document holds the state of the filesystem. A limit here is a limit the
project knows about, and each one has a reason.

## How to call the program

The form follows the other mount programs of FreeBSD. A device comes first,
and a folder comes second:

```
mtpfs ugen0.11 /mnt/phone
```

| Form                            | What it does                         |
| ------------------------------- | ------------------------------------ |
| `mtpfs -l`                      | Lists each device that gives MTP     |
| `mtpfs <folder>`                | Mounts the first device it finds     |
| `mtpfs <node> <folder>`         | Mounts one named device              |
| `mtpfs <node> <folder> -f`      | Stays in the foreground              |
| `umount <folder>`               | Stops the mount                      |

A node name takes two forms, `ugen0.11` and `/dev/ugen0.11`.

A file manager stops the mount correctly. Nautilus calls `umount`, which closes
the MTP session on the cellphone. A signal to the program does not close the
session, and the next mount then needs a repair.

### Two cellphones at one time

Each mount holds one session, on one device. Two mounts therefore hold two
cellphones:

```
mtpfs ugen0.11 /mnt/samsung
mtpfs ugen0.12 /mnt/moto
```

A copy from one mount to the other works. The bytes go through the host, and a
test compares a SHA-256 sum.

An earlier version took no device name, and found the first device. Two mounts
then held the same cellphone. A name gives the device a caller wants.

## What works

| Operation                  | Command        | State |
| -------------------------- | -------------- | ----- |
| Mount                      | `mtpfs`        | yes   |
| Stop the mount             | `umount`       | yes   |
| Stop the mount from a GUI  | file manager   | yes   |
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

### A listing costs three requests

`GetObjectPropList` gives one property of every object in a folder, in one
request. A listing needs three properties:

1. The name of the object.
2. The size of the object.
3. The format of the object, which says whether the object is a folder.

A folder therefore costs three requests, and the count does not grow with the
count of objects.

The older way costs one request for each object. `GetObjectHandles` gives the
handles in one request. `GetObjectInfo` then gives the name and the size of one
object. A folder with 229 objects costs 230 requests.

A measurement on a Samsung, for a folder of 229 objects:

| Way                                | Time   |
| ---------------------------------- | ------ |
| One request for each object        | 0.73 s |
| Three properties, three requests   | 0.21 s |

Both ways give the same listing. A test compares the name and the size of each
object, on a Samsung and on a Motorola.

A device that reports `GetObjectPropList` gets the fast way. A fault turns the
fast way off for the rest of the session, and the older way then answers.
`BSDROID_NO_PROPLIST=1` turns the fast way off from the start.

#### Two corrections

An earlier version of this document gave an estimate. A folder of 500 objects
takes about 10 seconds, at about 20 milliseconds for each request. The
measurement says 3.2 milliseconds for each request. The estimate was wrong by a
factor of six. The old way was never as slow as this document said.

The first version of the fast way asked for every property in one request. That
version was slower than the old way, at 1.99 seconds. The device sends each
date, and each identifier of 128 bits, for each object. A request that names
one property sends only the bytes a listing needs.

### The folder comes back in its own listing

A request with a depth of 1 gives the folder, and the children of the folder. A
listing needs the children alone.

A Samsung answers this way for a request that names one property. The answer
holds 230 objects for a folder of 229 files, and the extra object is the
folder. A file manager then shows `DCIM` inside `DCIM`.

The mount removes the folder from the answer. A test covers the rule.

### A listing does not change

The filesystem reads a folder one time, and keeps the answer while the mount
runs.

A file that another program adds to the device does not appear. A write through
the mount does update the listing of that folder.

Stop the mount and start the mount again to see a change from another program.

## What costs memory

A program closes a new file, and the file then goes to the device. MTP needs
the size of a file before the bytes. A program gives no size in advance.

The host therefore holds the bytes until the close. The bytes go to a spool
file on a disk, and not to memory. A copy of a file of 649 MB costs about 6 MB
of memory, and 649 MB of disk.

An earlier version held the bytes in memory. A copy of a file of 4 GB then
needed 4 GB of memory.

The spool file goes in one of three folders. The mount takes the first folder
in this list that has a path:

1. The folder in `BSDROID_SPOOL`.
2. The folder in `TMPDIR`.
3. `/var/tmp`.

`/var/tmp` is the default because `/tmp` on some hosts is in memory. A spool
file in memory gives back the cost this design removes. Set `BSDROID_SPOOL` to
a folder with free space for the largest file you copy.

The mount removes the spool file after the send, and after a fault.

A read costs 4 MB, and not the size of the file.

## Three faults at a packet boundary

A USB packet holds 512 bytes at high speed, and 1024 bytes at super speed. A
count of bytes that is a multiple of the packet size found three faults. Each
one is now a test in `crates/mtpfs/src/backend.rs`.

### A read lost the last byte

A Samsung stops on two conditions at once. The last packet of a partial read is
full, and the read reaches the end of the file. The workaround takes one byte
from the read.

An earlier version gave the short answer to the caller. A file of 307200 bytes
therefore read back as 307199 bytes. The count 307200 is 600 packets of 512
bytes.

The read now takes the last byte in a second request. The count of one is not a
multiple of the packet size, so the fault does not happen again.

### A write stopped the device

A data phase that ends on a packet boundary needs a packet of zero bytes. The
device counts packets, and a full last packet says that more bytes follow. The
device then waits, and the transfer stops.

A file of 524276 bytes gives a data phase of 524288 bytes, which is 1024
packets. That file stopped the device.

The host now sends a packet of zero bytes for a data phase that is a multiple
of the packet size.

### A read crossed the end of the cache

The cache holds 4194304 bytes. A read that starts inside the cache, and ends
after the cache, got only the part the cache holds.

The kernel takes a short answer as the end of the file. A program that reads
the file then got wrong bytes after 4194304 bytes.

`cp` reads on a boundary of 65536 bytes, and 65536 divides 4194304. `cp` never
crossed the end of the cache, and `cp` gave the right bytes. `sha256` reads
with a different count, and `sha256` gave a different answer for the same file.
The two answers found the fault.

The read now fills the whole request. The cache gives the first part, and the
device gives the rest.

### What the tests cover

Each fault has a test with the number that found the fault. One test adds the
two parts of a split read, and checks that the total is the count the caller
asked for.

## A device that does not take a write

The Cyrus CS 24 reads, and the Cyrus CS 24 does not write.

| Operation on the Cyrus CS 24        | State        |
| ----------------------------------- | ------------ |
| Find the device                     | yes          |
| List a folder                       | yes          |
| Read a file                         | yes          |
| Make a folder                       | yes          |
| Report the free space               | yes          |
| Write a file                        | **not sure** |

A read is correct. Five reads of a file of 16896994 bytes give the same
SHA-256 sum. A listing is correct, and the two ways of a listing agree.

A write stops in the data phase of `SendObject`. The host reports a timeout on
the bulk endpoint. The session then holds an open transaction, and the
next operation gets the code 0x2002.

The result of a write is not the same each time. One size gives a fault in one
test, and no fault in the next test. A count of writes at 131072 bytes gave 0
of 6, then 1 of 6, then 6 of 6 in an earlier test.

### What did not repair the fault

| Test                                     | Result       |
| ---------------------------------------- | ------------ |
| A deadline of 60 seconds                 | no change    |
| A write of 16384 bytes for each transfer | no change    |
| A USB reset at the close of the session  | 1 of 6       |
| A power cycle of the cellphone           | no change    |

The size of the file is not the cause. A file of 98304 bytes and a file of
131072 bytes passed in the same test that a file of 32768 bytes failed.

A failed write leaves no object on the device. The folder holds the same
objects before the test and after the test.

### What `libmtp` knows

`libmtp` holds this entry for the chip:

```c
{ "MediaTek Inc", 0x0e8d, "MT65xx/67xx (MTP mode)", 0x2008,
    DEVICE_FLAGS_ANDROID_BUGS },
```

`DEVICE_FLAGS_ANDROID_BUGS` holds six flags. Two of the six touch this work:

- `DEVICE_FLAG_FORCE_RESET_ON_CLOSE` says the device needs a USB reset after
  each connection. The comment says that some devices do not like a reset, so
  `libmtp` does not reset by default. A test of the reset gave 1 of 6.
- `DEVICE_FLAG_BROKEN_MTPGETOBJPROPLIST` says `GetObjectPropList` is broken on
  this chip. This project uses that operation for a listing. The listing of
  this device is correct, and matches the older way, so this project keeps the
  operation. A later report can change the rule.

No flag in the database describes a write that stops.

### What another program does

`aft-mtp-mount` does not list this device. The mount starts, and the first
listing gives `Device not configured`.

`aft-mtp-mount` stops. This project then reads the device, and lists the
device. A read gives the correct SHA-256 sum.

### Three switches for a report

A person with a device that does not write can try these switches. None of the
three repaired the Cyrus CS 24, and each one is a test for a new device.

| Switch                     | What the switch does                     |
| -------------------------- | ---------------------------------------- |
| `BSDROID_TIMEOUT=60`       | Gives a deadline of 60 seconds           |
| `BSDROID_WRITE_CHUNK=16384` | Writes 16384 bytes for each transfer    |
| `BSDROID_USB_RESET=1`      | Sends a USB reset at the close of the session |

A USB reset can change the node name of the device. Read the name again with
`mtpfs -l` after a reset.

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
