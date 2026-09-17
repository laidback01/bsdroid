# The design of the filesystem

This document records the choices for a FUSE filesystem, and the measurement
behind each choice. A choice with no measurement is marked.

## What the devices allow

`mtpprobe caps` reads the operations a device supports:

| Need                        | Samsung SM-S901U | Motorola Moto G (5) |
| --------------------------- | ---------------- | ------------------- |
| Read part of a file         | yes              | yes                 |
| Write a new file            | yes              | yes                 |
| Remove a file               | yes              | yes                 |
| Read a folder in one call   | yes              | yes                 |
| Operations in total         | 35               | 43                  |

Both devices allow a filesystem that reads and writes.

### The partial read is the important one

A read asks for an offset and a length. `GetObject` gives a whole file, so a
read of 100 bytes from a file of 4 GB costs 4 GB without a partial read.

Both devices support `GetPartialObject64`, which takes a 64 bit offset. The
filesystem uses that operation.

A device with no partial read needs a different design, with a copy of each
file on the disk of the host. The project has no such device to test.

### A fault in the partial read of a Samsung

The `libmtp` database holds a fault for the Samsung entry 0x6860, which is a
test device of this project:

```
When GetPartialObject is invoked to read the last bytes of a file
and the amount of data to read is such that the last USB packet
sent in the reply matches exactly the USB 2.0 packet size, then
the Samsung Galaxy device hangs, resulting in a timeout error.
```

The packet size of the test devices is 512 bytes. When the last packet of an
answer holds 512 bytes, a read of the end of a file hangs the device.

The filesystem must avoid the condition. The rule:

1. Work out the size of the last packet of the answer.
2. When the size is 512 bytes, and the read reaches the end of the file, ask
   for one byte less.
3. Read the last byte in a second request.

This project did not find the fault, because this project never read the end of
a file with a partial read. A test must cover the case, and the test needs a
Samsung.

See `docs/08-what-libmtp-knows.md`.

## One session, and not one for each operation

A session costs about 46 milliseconds to open and close:

| Device              | One open and close cycle |
| ------------------- | ------------------------ |
| Samsung SM-S901U    | 48 ms                    |
| Motorola Moto G (5) | 46 ms                    |

A filesystem does many operations. A session for each operation adds 46
milliseconds to each one, so the filesystem holds one session open.

### A correction

An earlier version of this project reported 23 milliseconds for one device and
270 for another, and used the difference to argue for one session. The numbers
came from two versions of the project, and the difference was a fault in this
project. See `docs/06-the-reset-that-breaks.md`.

The choice of one session is still right, and the old reason was wrong. The
right reason is the 46 milliseconds, which is large next to a read.

## One thread

MTP holds one session, and a session does one operation at a time. A second
thread must therefore wait.

The filesystem runs in one thread, and the design needs no lock. FUSE takes an
option for this.

## The tree

MTP gives each object a handle, which is a number. MTP gives each object a
parent handle. The objects form a tree, and the root has no parent.

A filesystem needs a path. The `tree` module holds the map from a path to a
handle, and the module is pure. A test runs the module with no device.

### The cache

`GetObjectHandles` reads the children of one folder. The answer does not change
often, and a read of a folder costs a request.

The tree keeps each listing. The tree does not yet drop a listing after a time,
and a file that another program adds does not appear. This is a known limit,
and a later version fixes it.

## The rate of a copy

FUSE asks for 131072 bytes at a time. FreeBSD sets the size, and a filesystem
does not choose it.

Each request to the device costs about 18 milliseconds. A copy of 450 MB needs
3440 requests, and the copy then takes 63 seconds. That rate is 7.2 MB each
second, and the link gives 32 MiB each second.

The host therefore reads 4 MB in advance, and keeps the rest for the next read.
A copy reads a file from the start to the end, so the next read almost always
follows the last one.

| Read ahead | Time for 450 MB | Rate           |
| ---------- | --------------- | -------------- |
| none       | 63 s            | 7.2 MB/s       |
| 4 MB       | 15 s            | 29.4 MB/s      |

The second rate is close to the 32 MiB each second that a direct copy reaches,
so the link is again the limit.

The cache holds one part of one object. A program that reads two files at once
therefore loses the cache on each change. A later version holds more than one
part.

## What the first version does

| Operation | The first version |
| --------- | ----------------- |
| List a folder | yes           |
| Read a file   | yes           |
| Write a file  | no            |
| Remove a file | no            |
| Rename a file | no            |

A read only filesystem is the first version, because a read only filesystem
cannot lose a file. The devices allow a write, and a write comes later.

## The library

FreeBSD ships `libfuse` version 2 and version 3. The project uses version 3.

The project generates the bindings with `bindgen`, and keeps the output in the
tree. A user then needs no `bindgen`. The same rule applies to `libusb20`. See
`tools/regen-bindings.sh`.

## A write must report its fault from flush, and not from release

MTP gives the size of an object before the bytes. FUSE gives the bytes with no
size in advance. The mount therefore writes to a spool file on the host. When the program
closes the file, the mount sends the whole object to the cellphone.

The first version sent the object from `release`. That is the wrong callback.
The header of `libfuse` says:

```text
The return value of release is ignored.
```

Every fault on the way to the cellphone went nowhere. A copy that never
reached the device reported success.

### The measurement

A file above the 4 GiB limit of MTP fails before the host sends one byte. The
test therefore needs no cable and no large transfer. The steps are open, ftruncate
and close. Only `close` can carry the answer.

| Version         | What `close` gave                | What the mount wrote to the log |
| --------------- | -------------------------------- | ------------------------------- |
| send in release | success                          | cannot write ... 5368709120 bytes |
| send in flush   | Input/output error, errno 5      | cannot write ... 5368709120 bytes |

Both versions knew. Only the second one told anybody.

### How this was found

A person pulled the USB cable in the middle of a write. The mount wrote

```text
mtpfs: cannot write /bsdroid-test/chaos.bin: SendObject: ... error
```

and `cp` gave the exit code 0 for the same file.

No test found this. The parsers had tests, the session had tests, and the
tests all passed. A callback gives its fault to a value that the kernel throws away. That
callback looks correct in every test.

### The rule

Send the object from `flush`, which runs on `close`, and whose answer the
kernel passes to the program.

`dup` and `fork` both make a second descriptor, so `flush` can run more than
one time for one open file. The first run sends the object and keeps the
answer. A later run gives the same answer and sends nothing.

A program can close a file with no flush. `release` still sends an object that
`flush` never sent, for that case. That send cannot report a fault. This is the
whole reason the work moved.

## A close must know which handle it closes

`flush` and `release` both run for each close. An earlier version looked at the
path alone, and a path can be open more than one time.

`cp` onto a file that is already there shows the fault. The trace of the three
test cellphones holds this order:

```
open /bsdroid-edit/a.txt flags=0x1   the writer
truncate /bsdroid-edit/a.txt size=0  the writer empties the file
open /bsdroid-edit/a.txt flags=0x0   a reader of the same path
write /bsdroid-edit/a.txt            no spool file, so EROFS
```

The close of the reader reached `flush`, which sent a file the writer had not
finished. `release` then removed the spool file. The next write found no spool
file and answered EROFS, and the cellphone held an empty file.

`fuse_file_info.fh` holds 64 bits, and an object handle holds 32 bits. The host
therefore marks a handle that writes with bit 32, and `flush` and `release` act
for a marked handle alone. The mark comes from the flags of that one `open`
call. The mark does not come from the state of the path. A reader of a path
that a writer holds open must stay unmarked.

The probe in `tools/` measures 19 checks. The code before this change passed 4
and failed 17. The code after it passes 19 on all three test cellphones.

## A write that failed must not become a send

`flush` sends the spool file. An earlier version sent the spool file whatever
happened before, and a write that failed still reached `flush`.

The measurement used a memory disk of 1.6 MB as the spool folder, and a file of
5 MB:

| Version | What `cp` said            | What the cellphone held |
| ------- | ------------------------- | ----------------------- |
| before  | Input/output error        | 1638400 bytes           |
| after   | No space left on device   | no object               |

`Pending` now holds the number of the fault. `send_pending` sees that number,
and stops. The number also goes back to the caller in place of EIO. A program
that sees ENOSPC can tell a person what to do.

A test with a file of zero bytes does not measure this. FreeBSD `cp` looks for
blocks of zeros, writes none of them, and calls `ftruncate` for the size. The
spool file is then a sparse file of 50 MB on a disk of 1.6 MB, and every check
passes for the wrong reason. Use `dd if=/dev/random` for a test of this kind.
