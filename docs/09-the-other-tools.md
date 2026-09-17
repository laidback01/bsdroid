# The other tools on FreeBSD

FreeBSD ports hold four programs that mount an Android device over MTP. This
document records what each one does on the test system.

Each of these programs is a good program, and each one works on Linux. This
document is not a judgment of the work of another person.

The document exists for one reason. A reader can fairly ask why this project
exists, because MTP over FUSE is not a new idea. The answer needs a
measurement, and not an opinion.

The author of this project used these programs over several years, and met the
same trouble each time. The measurements below put a number on that experience.

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

## One cause, and three programs

The three programs come from different people, and the three hold different
code. Each one fails on FreeBSD, and each one reaches the telephone through
`/usr/lib/libusb.so.3`.

That file is the compatibility layer. The layer holds the defect in
`docs/00-why.md`.

A program that uses the layer cannot set a deadline for a transfer. The author
of such a program writes correct code, and the code still stops.

## The road this project took

There are two roads:

1. Repair the compatibility layer. The repair then helps every program on
   FreeBSD.
2. Do not use the layer.

This project took the second road, and used `libusb20` from the base system.
The choice is not a judgment of the first road, and the first road is still
worth the work.

The FUSE part of this project is not new. The transport is.

## A fair note about aft-mtp-mount

A reader who needs a listing today, or a small file, has a working answer in
the ports tree. `aft-mtp-mount` does that work, and the program holds a
complete MTP implementation.

The fault above needs three things together: that program, this operating
system, and a large file. Two of the three are not the work of the author.
