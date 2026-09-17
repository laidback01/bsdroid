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
