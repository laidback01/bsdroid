# bsdroid

Android device support for FreeBSD.

## Status

Early. The project builds the first layer. No tool is ready for use.

## The problem

An Android phone does not mount reliably on FreeBSD. The available MTP
filesystems stop and use 100% of one CPU core. The cause is a loop in the
`libusb-1.0` compatibility layer. `docs/00-why.md` records the measurement and
the cause.

## The goal

A FreeBSD user connects an Android phone and reads the files. The user does not
change a setting on the phone. The user does not enable developer mode.

MTP needs no setup on the phone, so MTP is the transport for the product.
`adb` needs developer mode, so the project uses `adb` only as a test reference.

## The approach

The project does not use `libusb-1.0`. The project uses `libusb20`, which
FreeBSD ships in the base system. `libusb20` gives a timeout that the caller
controls.

## Crates

| Crate         | Purpose                                      | State           |
| ------------- | -------------------------------------------- | --------------- |
| `ptp-proto`   | PTP wire format. No I/O and no dependency.   | tests pass      |
| `usb-freebsd` | USB transport over `libusb20`.               | tests pass      |
| `mtpprobe`    | Reports what a device does, and where it stops. | works on three phones |
| `mtpfs`       | Mounts a device as a folder. Read only.      | reads verified  |

## mtpfs

```
cargo build
./target/debug/mtpfs /path/to/a/folder
```

The mount is read only. A read gives the same bytes as a copy over `adb`, and
a test compares a SHA-256 sum.

To stop the mount:

```
umount /path/to/a/folder
```


## mtpprobe

```
cargo run --bin mtpprobe
```

The program finds an MTP device, reads the storage, and reports each step with
a time. Every transfer has a 5 second deadline, so the program always stops.

The program also sends the operation sequence that stops `simple-mtpfs` and
`jmtpfs`. On a Samsung SM-S901U each step takes 3 milliseconds or less, and the
program does not stop. `docs/00-why.md` gives the numbers.

## Tests

```
cargo test
```

The tests need no phone. The test data comes from a real capture of a Samsung
SM-S901U. `crates/ptp-proto/tests/fixtures/README.md` records the source.

## Test hardware

The project needs reports from many devices. At present the project has three:

| Item                     | Samsung SM-S901U | Motorola Moto G (5) | Cyrus CS 24    |
| ------------------------ | ---------------- | ------------------- | -------------- |
| Chip maker               | Qualcomm         | Qualcomm            | MediaTek       |
| The name in the shop     | Galaxy S22       | Moto G (5)          | NUU B20        |
| MTP interface class      | 0x06/0x01/0x01   | 0xff/0xff/0x00      | 0x06/0x01/0x01 |
| The name of the interface | `MTP`           | `MTP`               | `MTP`          |
| Interfaces in this mode  | 4                | 2                   | 1              |
| An adb interface         | yes              | yes                 | no             |
| Storage on attempt 1     | no               | yes                 | yes            |
| Super speed capability   | yes              | no                  | no             |
| Objects on the storage   | 2059             | 57                  | 8656           |
| Rate, 64 KiB reads       | 32.1 MiB/s       | 28.2 MiB/s          | 39.4 MiB/s     |
| One open and close cycle | 48 ms            | 46 ms               | not measured again |
| Operations supported     | 35               | 43                  | not measured yet   |
| A filesystem can read part of a file | yes  | yes                 | not measured yet   |
| A filesystem can write   | yes              | yes                 | not measured yet   |

An earlier version of this table gave 23 ms for the Samsung and 270 ms for the
Motorola. Those two numbers came from two versions of this project, and one
version held a drain that cost 250 milliseconds. The numbers did not compare.

The two values above come from the same version, and the two devices agree. See
`docs/06-the-reset-that-breaks.md`.

### What the three devices settle

Two rules came from one device, and a second device broke each one:

- The class of the MTP interface. Two devices use the standard class, and one
  uses a vendor class. See `docs/05-finding-the-interface.md`.
- The wait before a storage appears. One device waits, and two do not. See
  `docs/01-cold-start.md`.

One rule holds on all three devices, and the rule now has weight:

- `GetObjectHandles` with 0x00000000 gives every object, and with 0xffffffff
  gives the root folder. The counts are 2059 and 13, 57 and 11, 8656 and 14.
  See `docs/03-object-handles.md`.

The name of the interface is `MTP` on all three devices. The name holds across
two chip makers, and across two classes.

A fourth device is more use to this project than a hundred more runs on these
three.

### How to send a report

```
cargo build
sh tools/capture-device.sh <a name for your device>
```

The script hides each file name. Read the file in `docs/captures` before you
send the file.

## Documentation language

The documentation uses Simplified Technical English (ASD-STE100). Many readers
of this project read English as a second language.

A check enforces the rules. The check does not depend on the judgment of the
writer:

```
git clone --depth 1 https://github.com/AminBlg/SimpleEnglish.git ../SimpleEnglish
sh tools/lint-docs.sh
```

The check reads every Markdown file. If a file holds a violation, the check
fails. The linter comes from the SimpleEnglish project, under the MIT licence.
This repository does not copy the linter.

The check measures the mechanical rules:

- sentence length
- contraction
- perfect tense
- semicolon and em dash
- Latin abbreviation
- vague word
- trailing condition

The check does not measure the approved word list. ASD controls the
distribution of the list, so no project can ship the list. A reader who owns
the Issue 9 specification can make a local word list. The `tools/ste-dictionary`
directory of the SimpleEnglish project shows how.

## Licence

BSD 2-Clause.
