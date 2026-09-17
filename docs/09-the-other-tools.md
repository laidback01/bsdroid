# The other tools on FreeBSD

FreeBSD ports hold four programs that mount an Android device over MTP. This
document records what each one does on the test system.

A project that repeats the work of another project wastes the time of a reader.
This document therefore gives the measurement, and not an opinion.

## The test

- FreeBSD 15.1-RELEASE-p2, amd64
- Samsung SM-S901U, file transfer mode
- The same three files each time
- The host compares a SHA-256 sum with the sum the telephone gives over `adb`

## The result

| Program            | Mounts | Lists a folder | 290 KB | 8 MB | 450 MB |
| ------------------ | ------ | -------------- | ------ | ---- | ------ |
| `simple-mtpfs`     | no     | no             | no     | no   | no     |
| `jmtpfs`           | no     | no             | no     | no   | no     |
| `aft-mtp-mount`    | yes    | yes            | yes    | yes  | no     |
| `mtpfs` of this project | yes | yes           | yes    | yes  | yes    |

### simple-mtpfs and jmtpfs

Both programs stop and use 100% of one CPU core. Neither program mounts.

`docs/00-why.md` holds the measurement and the cause. Both programs use the
`libusb-1.0` compatibility layer, and that layer holds the fault.

### aft-mtp-mount

The program works for a listing and for a small file. The program comes from
the `android-file-transfer-fuse` port, and the program holds its own MTP code.

A copy of 450 MB fails:

```
cp: .../20260910_101106.mp4: Socket is not connected
```

The copy gives 423362560 bytes of 450943822, which is 94%, after 59 seconds.

The failure then holds the device. Each later read gives `Device not
configured`, and a new mount fails:

```
connect failed: libusb_bulk_transfer(...): LIBUSB_ERROR_TIMEOUT
```

The program links `/usr/lib/libusb.so.3`, which is the compatibility layer.

### The filesystem of this project

The same 450 MB file, on the same telephone, in the same session:

| Item          | Value                          |
| ------------- | ------------------------------ |
| Time          | 15 seconds                     |
| Rate          | 29.4 MB each second            |
| Size          | 450943822 bytes, which is right |
| SHA-256       | agrees with the telephone      |

## The repair of a device that another program left

`aft-mtp-mount` did not mount the device again after the failure. Two runs
gave the same timeout.

The filesystem of this project mounted the same device on the second try, and
then copied the 450 MB file with the correct sum.

`docs/06-the-reset-that-breaks.md` holds the work. The host does three things:

1. The host clears a halt on each endpoint.
2. The host reads and drops the bytes the device still holds.
3. The host closes a session that an earlier program left open.

## What this project adds

A reader can fairly ask why this project exists, because MTP over FUSE is not a
new idea.

The answer is the transport. Each other program uses the `libusb-1.0`
compatibility layer of FreeBSD. This project uses `libusb20`, which FreeBSD
ships in the base system, and which gives a deadline that the caller controls.

The FUSE part of this project is not new. The transport is.

## A fair note

`aft-mtp-mount` is a good program, and the program works on Linux. The fault
above is a fault of the combination: that program, this operating system, and a
large file.

A reader who needs a listing and a small file today has a working answer in the
ports tree. A reader who needs a large file does not.
