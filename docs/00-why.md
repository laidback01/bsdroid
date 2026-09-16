# Why this project exists

This document records a defect. The defect is the reason for the project.

## Summary

An Android phone does not mount on FreeBSD. The two available MTP filesystems
both stop and use 100% of one CPU core. The phone is not at fault. The
filesystems are not at fault. The fault is in the compatibility layer below
both of them.

## The symptom

A user mounts a Samsung SM-S901U with `simple-mtpfs`. The command does not
return. The mount point becomes unusable:

```
d---------   0 root wheel  0 Dec 31  1969 phone
```

A process that reads the mount point stops and does not answer a signal. The
`simple-mtpfs` process uses 99% of one core. The memory of the process does not
grow, so the process does no work.

## The measurement

Test system:

- FreeBSD 15.1-RELEASE-p2, amd64
- `fusefs-simple-mtpfs` 0.4.0
- `fusefs-jmtpfs` g20190420
- `libmtp` 1.1.23
- Samsung SM-S901U, Android 16, MTP mode

A system call trace of a mount attempt holds 455262 lines for 20 seconds. That
count is about 23000 system calls each second. The trace repeats three calls:

```
poll({ 5/POLLIN 7/POLLIN|POLLOUT|POLLRDNORM },2,-1) = 1 (0x1)
read(5,0x820790528,8)              ERR#35 'Resource temporarily unavailable'
ioctl(7,USB_FS_COMPLETE,0x82079049f)   ERR#16 'Device busy'
```

The trace holds 106 successful USB transfers. All 106 occur in the first 1218
lines. The remaining 454044 lines do no work.

## The cause

Read the three calls in order:

1. `poll` reports that a file descriptor is ready. The timeout argument is
   `-1`, which means no timeout.
2. `read` finds no data and returns `EAGAIN`.
3. `ioctl` asks for the result of the transfer. The transfer is not complete,
   so the call returns `EBUSY`.

The loop then starts again. The `poll` call reports ready each time, because
nothing clears the ready state. The loop never stops.

FreeBSD has no native `libusb-1.0`. The file `/usr/lib/libusb.so.3` is a
compatibility layer over `libusb20`, which is the native FreeBSD USB library.
`libusb-1.0` has an event loop design that comes from Linux. The compatibility
layer must imitate that design.

`libmtp` sets a timeout for each USB transfer. The timeout never takes effect,
because control never returns from the compatibility layer to `libmtp`. A
timeout cannot help a caller that the library never calls back.

## The trigger

A capture with `LIBMTP_DEBUG=9` shows the last operations before the loop:

| Transaction | Operation      | Code   | Result |
| ----------- | -------------- | ------ | ------ |
| 33          | GetStorageIDs  | 0x1004 | OK     |
| 34          | GetStorageInfo | 0x1005 | OK     |
| 35          | CloseSession   | 0x1003 | OK     |

The device answers `CloseSession` correctly. The loop starts after the answer.

### A first idea, which a test disproved

The first version of this document gave a reason for the loop. `simple-mtpfs`
opens a PTP session, closes the session, and opens a second session. The idea
was that the phone stops at the second open.

`mtpprobe` tested the idea against the same phone. The program sent this exact
sequence through `libusb20`:

| Step | Operation      | Time  | Result |
| ---- | -------------- | ----- | ------ |
| 1    | OpenSession    | 2 ms  | OK     |
| 2    | GetStorageIDs  | 1 ms  | OK     |
| 3    | GetStorageInfo | 3 ms  | OK     |
| 4    | CloseSession   | 0 ms  | OK     |
| 5    | OpenSession    | 1 ms  | OK     |

Step 5 is the step the idea said must fail. Step 5 took 1 millisecond and gave
OK.

**The idea was wrong.** The phone accepts a second PTP session. The phone is
not the cause, and no phone quirk is the cause.

### What the test leaves

The loop is a fault in the `libusb-1.0` compatibility layer alone. The PTP
session cycle does not cause the loop.

One difference remains between `mtpprobe` and `simple-mtpfs`. `mtpprobe` opens
the USB device one time. `simple-mtpfs` opens the USB device, closes the USB
device, and opens the USB device a second time. A USB device open is not a PTP
session open.

The next test must repeat the USB device open and close cycle. This document
does not yet say that the cycle is the cause, because no test shows it.

## What the project rules out

| Cause                | Test                                          | Result       |
| -------------------- | --------------------------------------------- | ------------ |
| File permissions     | User is in group `operator`, device is `0660` | Not the fault |
| A second process     | `fstat` shows no other USB file descriptor    | Not the fault |
| A device fault       | `mtp-detect` reads the full device            | Not the fault |
| A `simple-mtpfs` bug | `jmtpfs` fails the same way                   | Not the fault |
| A libmtp bug         | `adb` moves data over USB with no loop        | Not the fault |

The last row is important. `adb` uses USB on the same machine and the same
phone, and `adb` does not loop. The defect needs the MTP transfer pattern.

## What the project does about the defect

The project does not use `libusb-1.0`. The project uses `libusb20`, which
FreeBSD ships in the base system. `libusb20` has these functions:

- `libusb20_tr_set_timeout`
- `libusb20_tr_setup_bulk`
- `libusb20_tr_start`
- `libusb20_tr_drain`

These functions give a timeout that the caller controls. A program that uses
them cannot enter the loop above.

## Rules that come from the defect

Each rule below is a direct result of the measurement.

1. A loop must not depend on the device for the end condition. Every loop has a
   count limit or a deadline.
2. Every transfer has a timeout, and the program honors the timeout.
3. A parse function never panics. A parse function returns an error.
4. The protocol code does no I/O. A test runs the protocol code with no device.
5. A tool reports where a transfer stops. A user who reports a fault must not
   need a system call trace.

Rule 5 is the reason the first program is a diagnostic tool.
