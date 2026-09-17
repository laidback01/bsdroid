# The repair that breaks a working device

The still imaging class defines request 0x66, the device reset request. The
request puts the protocol state of a device back to the start.

This project sent the request at each open, and the choice was wrong.

## Why the project sent the request

A defect in this project stopped a data phase in the middle. The device then
held 6281974 bytes, answered no command, and kept a session open. A cable
disconnect did not repair the device.

Request 0x66 repaired the device. Before the request, each command reached the
deadline. After the request, a command took 5 milliseconds.

The project then sent the request at each open. The reasoning: a request that
repairs a broken device costs little on a device that works.

The reasoning was wrong.

## The measurement that showed the fault

A Cyrus CS 24 in PTP mode, six runs of `mtpprobe probe`:

| The host sends 0x66 | Runs that passed |
| ------------------- | ---------------- |
| yes                 | 1 of 5           |
| no                  | 6 of 6           |

The same device in MTP mode answered each run, with the request and without.
The fault needs PTP mode and the request together.

A first idea said that the device needs time after the request. A wait of 400
milliseconds gave 1 run of 5, which is the same result. The idea was wrong, and
the wait went away.

## The rule

A device reset request is a repair. A repair goes to a device that needs the
repair.

A user now asks for the request, and the project then sends the request:

```
BSDROID_PTP_RESET=1 mtpprobe probe
```

The tool names the variable in the message about a device that holds a session.

## What this cost

The project spent several runs on a Cyrus CS 24 that looked broken. The device
worked. The host broke the device each time, and then reported the fault as a
fault of the device.

A diagnostic tool that changes a device, and then measures the device, measures
itself.

## The rule that comes from this

Do no work at the start that a device does not need. A step that repairs a
broken device is not free on a device that works.

The project checks this rule for each step at the start of a session:

| Step at the start          | Does a working device need the step? |
| -------------------------- | ------------------------------------ |
| Read the configuration     | yes, to find the interface           |
| Read an interface name     | only for a vendor class              |
| Clear a halt on an endpoint | no, and the step is safe            |
| Read and drop stale bytes  | no, and the step is safe             |
| Device reset request, 0x66 | no, and the step is NOT safe         |

The last row is the one this document exists for.

## The same fault, a second time

The rule caught a second step, and the second step was already in the project.

`Session::new` reads and drops the bytes a stopped program leaves behind. The
first version used a deadline of 250 milliseconds for each read. An endpoint
with nothing on it gives nothing, and the read waits for the whole deadline.

A session on a Samsung SM-S901U:

| The host drains at the start | Mean time for one cycle |
| ---------------------------- | ----------------------- |
| yes, 250 ms deadline         | 290 ms                  |
| no                           | 29 ms                   |
| yes, 15 ms first deadline    | 48 ms                   |

The first read now uses 15 milliseconds. A device that holds bytes answers at
once, because the bytes are already there. The host then uses a longer deadline
for the rest.

### What the fault did to a measurement

The project reported one cycle at 23 milliseconds for a Samsung, and 270
milliseconds for two other cellphones.

The 23 millisecond measurement came from a version with no drain. The 270
millisecond measurements came from a version with the drain. The numbers do not
compare, and the difference is the drain.

The project used that difference to argue about the design of a filesystem. The
argument rested on a number that the project made.

### The rule, again

Measure a change against the same code. A number from an older version of a
program is a number about that version.

## The other reset, which goes to the port

Request 0x66 is not the only reset. `libusb20_dev_reset` sends a reset to the
USB port. The device leaves the bus and comes back, in the state a user gets
after a connect. If `BSDROID_USB_RESET` is on, `mtpfs` sends this reset when a
session closes.

`libmtp` holds a flag for the same job, `DEVICE_FLAG_FORCE_RESET_ON_CLOSE`.
The flag is on the entry for the MediaTek chip 0x0e8d:0x2008.

### A reset needs root

An earlier comment in `usb-freebsd` said that `usbconfig` needs root for a
reset and that this call does not. That is wrong.

| Who runs it | What happens                                     |
| ----------- | ------------------------------------------------ |
| operator    | code -99, and the device stays on the bus        |
| root        | the device leaves and comes back in about 4600 ms |

The code is `LIBUSB20_ERROR_OTHER`, and not `LIBUSB20_ERROR_ACCESS`. The
number does not name the cause, so only the run as root gives the answer.

`BSDROID_USB_RESET` therefore does nothing for a user who is not root.

### Two of three cellphones leave file transfer mode

A port reset costs the user. Measured on three cellphones, each reset as root:

| Device                        | After the reset                        |
| ----------------------------- | -------------------------------------- |
| Motorola Moto G (5)           | the user set file transfer mode again  |
| Samsung SM-S901U              | the user set file transfer mode again  |
| Cyrus CS 24, 0x0e8d:0x2008    | stayed in file transfer mode           |

The device that does not care is the MediaTek chip, which is the one device
`libmtp` resets on close. A flag for one chip is the right shape for this. A
reset for every device is not.

### How the project got this wrong the first time

The project read `mtpfs -l` after a reset, saw all three devices, and reported
that no device left file transfer mode. The list was right and the conclusion
was wrong: a person had already set the mode again on two of them.

A device list says what the bus holds now. It does not say what a person had
to do to put it there. Ask the person.
