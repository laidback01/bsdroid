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
| Write onto a file          | `cp`           | yes   |
| Make a file shorter        | `truncate`     | yes   |
| Add to the end of a file   | `>>`           | yes   |
| Change a file in place     | `sed -i`       | yes   |
| Move a file onto a file    | `mv`           | yes   |
| Report the free space      | `df`           | yes   |
| Search a tree              | `find`         | yes   |

A read gives the same bytes as a copy over `adb`. A test compares a SHA-256 sum
for a file of 290 KB, and for a file of 450 MB.

## What does not work

### A change to a file that is on the device

This works. The text below says how, because the way the host does the change
decides what a fault leaves behind.

MTP has no operation that writes into the middle of an object, and no operation
that renames one object onto the name of another. A change to a file therefore
becomes a new object.

The host follows the shape of copy on write:

1. The host reads the file into a spool file on its own disk.
2. The program changes the spool file.
3. The old object on the cellphone moves to a name that ends with
   `.bsdroid-old-<number>`.
4. The host sends the spool file under the real name.
5. The host removes the old object.

A read of the whole file is the cost of a change to one byte. A change to one
byte of a file of 4 GB reads 4 GB and writes 4 GB.

ZFS makes step 4 and step 5 one atomic step. MTP gives no atomic step, so the
guarantee here is smaller:

- The host never removes the old contents before the new contents hold the real
  name.
- A mount that stops between step 4 and step 5 leaves two objects. The first is
  the new object under the real name. The second is the old object, under the
  `.bsdroid-old-<number>` name. A person sees both objects, and loses nothing.

A send that fails puts the old name back, so the folder looks as it did.

### The space a change needs

A change to a file needs space in two places:

| Place              | How much it needs               |
| ------------------ | ------------------------------- |
| The disk of the host | The size of the whole file    |
| The cellphone      | The size of the whole file, one more time |

The host checks both places before it starts. If the space is not there, the
host refuses with ENOSPC. The refusal comes before the read, and not after some
gigabytes.

`BSDROID_SPOOL` says which folder holds the spool file. The order is
`BSDROID_SPOOL`, then `TMPDIR`, then `/var/tmp`. A `/tmp` folder that lives in
memory is a poor choice for a file of some gigabytes.

The cellphone needs room for two copies, because the old copy stays until the
new copy is in place. A cellphone with 6 GB free cannot hold two copies of a
file of 4.2 GB. The host then takes the second way:

1. The host removes the old object first.
2. The host sends the spool file under the real name.

The spool file on the host is the only copy between step 1 and step 2. A send
that fails in that window keeps the spool file, and the log gives the name of
the spool file. Copy the file from there.

The log says which way the host took, so a person can see the difference:

```
mtpfs: /DCIM/movie.mkv needs 4200 MB, and the cellphone has 1800 MB free.
       The old copy goes first.
```

A file that fits in neither way gets ENOSPC, and the host changes nothing.

### A spool file that took only part of a file

A disk that fills in the middle of a write leaves a spool file with the wrong
contents. The host does not send that spool file.

An earlier version sent the part. A disk of 1.6 MB and a file of 5 MB left an
object of 1.6 MB on the cellphone, under the right name. `cp` reported
`Input/output error`. A part of a file under the right name is worse than no
file, because a person cannot see the difference.

The host now reports the number the disk gave. `cp` says
`No space left on device`, and the folder on the cellphone holds no new object.

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

## A cable that does not carry a write

A write needs a good cable. A read does not show the fault, and a charge does
not show the fault.

One test cellphone, the Cyrus CS 24, failed each write. The same cellphone read
each file correctly. Five reads of a file of 16896994 bytes gave the same
SHA-256 sum.

A new cable repaired the fault. No line of the program changed.

| Cable    | Writes that passed |
| -------- | ------------------ |
| The old cable | 0 of 10       |
| A new cable   | 10 of 10      |

The test writes a file of 1048576 bytes ten times, and compares a SHA-256 sum
for each write.

The new cable gives 10 of 10 in two configurations of the cellphone:

1. Developer options on, which gives the identifier 0x201d.
2. Developer options off, which gives the identifier 0x2008.

A user with no developer options therefore gets the same result. The cable is
the one thing that changed the count.

### What the cellphone reported

USB debugging gives a log from the cellphone. The log holds the cause:

```
E d.process.medi: Mtp got unexpected short packet
E MtpServer: Mtp receive file got error I/O error
W MtpServer: [MTP] got response 0x2002 in command MTP_OPERATION_SEND_OBJECT
```

A USB transfer ends with a short packet. A short packet in the middle of a
transfer tells the receiver that the data stopped. The MTP server of the
cellphone then stops the transfer, and answers 0x2002 to each later operation.

The host waits for an answer on the bulk in endpoint, and the answer does not
come. The host then reports a timeout. The timeout is the result, and not the
cause.

### Why the fault is hard to find

| Fact                                    | Effect on a test          |
| --------------------------------------- | ------------------------- |
| The cable charges the cellphone          | The cable looks good      |
| The link runs at 480 Mbps                | The speed looks good      |
| Each read gives the correct bytes        | The cellphone looks good  |
| The size of the file does not matter     | A test finds no pattern   |

The USB descriptor of the device also came back with a different product name
after a fault. The device left the bus and came back 11 times in one evening.

### What did not repair the fault

Each test below came from a document or from source code. None of the four
changed the count of writes that passed.

| Test                                     | Source of the idea                |
| ---------------------------------------- | --------------------------------- |
| A deadline of 60 seconds                 | `DEVICE_FLAG_LONG_TIMEOUT`        |
| A write of 16384 bytes for each transfer | `MTP_BULK_BUFFER_SIZE`            |
| A USB reset at the close of the session  | `DEVICE_FLAG_FORCE_RESET_ON_CLOSE` |
| A power cycle of the cellphone           | The usual first step              |

The three switches stay in the program. Each one gives a test for a new device.

| Switch                      | What the switch does                     |
| --------------------------- | ---------------------------------------- |
| `BSDROID_TIMEOUT=60`        | Gives a deadline of 60 seconds           |
| `BSDROID_WRITE_CHUNK=16384` | Writes 16384 bytes for each transfer     |
| `BSDROID_USB_RESET=1`       | Sends a USB reset at the close of the session |

A USB reset can change the node name of the device. Read the name again with
`mtpfs -l` after a reset.

### The rule for a report

A write that fails, and a read that passes, points at the cable. Change the
cable first. Send a report after the new cable fails too.

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

## What a pulled cable does

A person pulled the USB cable while the host read and while the host wrote.
The cellphone left the bus in the middle of a transfer, came back, and the
person repeated that many times. One cable in the set has a bad contact, and
that cable went in as well.

The question is the one `docs/00-why.md` asks. Does a command come back?

| Path  | Cellphone           | Mounts | Worked | Faults | Hangs | Wrong bytes | Lost |
| ----- | ------------------- | ------ | ------ | ------ | ----- | ----------- | ---- |
| read  | Cyrus CS 24         | 5      | many   | 5      | 0     | n/a         | n/a  |
| read  | Motorola Moto G (5) | 6      | many   | 4      | 0     | n/a         | n/a  |
| write | Cyrus CS 24         | 20     | 236    | 72     | 0     | 0           | 0    |
| write | Samsung SM-S901U    | 19     | 188    | 68     | 0     | 0           | 0    |
| write | Motorola Moto G (5) | 15     | 159    | 56     | 0     | 0           | 0    |

The three write rows ran at the same time, on one host, with three mounts
open. Each write copies 24 MiB and reads the file back to compare the sum. The
583 writes that worked therefore moved about 14 GB, and every one of them came
back with the bytes it went out with.

The slowest command took 2 seconds. The deadline is 45 seconds for a read and
120 seconds for a write.

A fault is a pass. The cellphone is gone, so a command that refuses is right.
A command that never comes back is the defect this project exists to avoid.

Every command came back. The slowest read took 1 second, and the deadline is
45 seconds. A read of a device that is gone does not wait for the deadline: it
fails at once, because the transfer reports that the device is gone.

The mount recovered by itself each time. The host found the cellphone again in
about one second after the cellphone came back.

### What this test found

The write test found a defect on its first cycle, and the defect was not in
the loop. `mtpfs` sent the object from the FUSE callback `release`, and the
kernel throws away the answer of `release`. A copy that never reached the
cellphone reported success. See `docs/07-filesystem-design.md`.

No test in this project finds that. A test cannot make hardware leave the bus.
When nothing fails, the callback looks correct.

`tools/chaos-cable.sh` holds the test. It needs a person with a cable.
