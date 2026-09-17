# bsdroid

Mount an Android cellphone as a folder, on FreeBSD.

## Why this exists

A FreeBSD desktop does not get on well with a modern Android cellphone. You
connect the cellphone, you try to mount it, and the mount stops. The mount
point then gives an error for each command, and one CPU core runs at 100%.

This project began with mild frustration at that, after several years of it.

The author used the programs below over that time, and met the same trouble
each time. On the day this project started, `simple-mtpfs` left a mount point
in this state:

```
d---------   0 root wheel  0 Dec 31  1969 phone
```

No command read the folder. No signal stopped the program.

### The state of things

FreeBSD ports hold three other programs that mount a cellphone over MTP. Each
one ran against a Samsung SM-S901U on the same host, with the same files:

| Program         | Mounts | Lists a folder | 290 KB | 8 MB | 450 MB |
| --------------- | ------ | -------------- | ------ | ---- | ------ |
| `simple-mtpfs`  | no     | no             | no     | no   | no     |
| `jmtpfs`        | no     | no             | no     | no   | no     |
| `aft-mtp-mount` | yes    | yes            | yes    | yes  | no     |

These are good programs, and each one works on Linux. The fault is not in them.

Each program reaches the cellphone through `/usr/lib/libusb.so.3`. FreeBSD has
no native `libusb-1.0`, so that file is a compatibility layer over the native
USB library. The layer holds a defect. When a transfer stalls, the layer polls
with no timeout, and the layer never gives control back. A program that uses
the layer cannot set a deadline, and cannot recover.

`docs/00-why.md` holds the measurement: 455262 system calls in 20 seconds, with
106 useful transfers among them.

### The choice this project made

There are two roads. Repair the compatibility layer, or do not use the layer.

This project does not use the layer. FreeBSD ships `libusb20` in the base
system, which is the native USB library, and which gives a deadline that the
caller controls. The USB code and the MTP code here are new.

That is not a criticism of the other programs. A repair of the compatibility
layer helps every program on FreeBSD, and the repair is still worth doing. This
project took the other road.

## What works now

| Operation              | State |
| ---------------------- | ----- |
| Mount                  | yes   |
| List a folder          | yes   |
| Read a file            | yes   |
| Write a new file       | yes   |
| Make a folder          | yes   |
| Remove a file          | yes   |
| Remove a folder        | yes   |
| Rename                 | no    |
| Change a file in place | no    |

A read gives the same bytes as a copy over `adb`. A test compares a SHA-256
sum, for a file of 290 KB and for a file of 450 MB.

## Build and use

```
pkg install fusefs-libs3
cargo build --release
./target/release/mtpfs /path/to/an/empty/folder
```

Before you start:

1. Connect the cellphone.
2. Unlock the cellphone.
3. Put the cellphone into file transfer mode.

To stop:

```
umount /path/to/an/empty/folder
```

The project also holds `mtpprobe`, which reports what a cellphone does, and
where a transfer stops. Run `mtpprobe --help` for the commands.

## Test hardware

Three cellphones, from three makers, with two chip makers:

| Cellphone           | Sold as    | Chip     |
| ------------------- | ---------- | -------- |
| Samsung SM-S901U    | Galaxy S22 | Qualcomm |
| Motorola Moto G (5) | Moto G5    | Qualcomm |
| Cyrus CS 24         | NUU B20    | MediaTek |

The three do not agree about much. Two use the standard USB class for MTP, and
one uses a vendor class. One waits before it reports a storage, and two do not.
Each one gives a different name to the same USB mode.

Each disagreement broke a rule that came from one cellphone. The files in
`docs/` hold the measurement for each one.

A fourth cellphone is more use to this project than a hundred more runs on
these three:

```
cargo build
sh tools/capture-device.sh <a name for your device>
```

The script hides each file name. Read the file in `docs/captures` before you
send the file.

## How this was built

A person and Claude Code wrote this together, over one long session. The person
supplied the cellphones, the cables, the hands, and the questions that broke
the wrong answers.

Several documents in `docs/` record a correction. The project measured one
device, drew a rule, met a second device, and found the rule wrong. Some of the
faults were in this project, and each document says so.

## Documentation

| File                               | Subject                                     |
| ---------------------------------- | ------------------------------------------- |
| `docs/00-why.md`                   | The defect in the compatibility layer       |
| `docs/01-cold-start.md`            | A cellphone that reports no storage at first |
| `docs/02-device-states.md`         | What a USB mode changes, and what it does not |
| `docs/03-object-handles.md`        | How a device lists the objects it holds     |
| `docs/04-file-transfer.md`         | The copy of a file, and the rate            |
| `docs/05-finding-the-interface.md` | Two ways a cellphone gives MTP              |
| `docs/06-the-reset-that-breaks.md` | A repair that breaks a working device       |
| `docs/07-filesystem-design.md`     | The design of the filesystem                |
| `docs/08-what-libmtp-knows.md`     | What the `libmtp` database already knew     |
| `docs/09-the-other-tools.md`       | The measurements of the other programs      |

The documentation uses Simplified Technical English (ASD-STE100), because many
readers of this project read English as a second language.
`tools/lint-docs.sh` checks the rules.

## Tests

```
cargo test
```

The tests need no cellphone. The test data comes from real captures, and
`crates/ptp-proto/tests/fixtures/README.md` records the source of each one.

## Licence

BSD 2-Clause.
