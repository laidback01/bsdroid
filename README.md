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
| `mtpprobe`    | Reports what a device does, and where it stops. | works on one phone |

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

The project needs reports from many devices. At present the project has two,
and the two disagree about three things:

| Item                    | Samsung SM-S901U | Motorola Moto G (5) |
| ----------------------- | ---------------- | ------------------- |
| Android                 | 16               | 8.1                 |
| MTP interface class     | 0x06/0x01/0x01   | 0xff/0xff/0x00      |
| The name of the interface | `MTP`          | `MTP`               |
| Storage on attempt 1    | no, on attempt 2 | yes                 |
| Super speed capability  | yes              | no                  |
| Rate, 64 KiB reads      | 32.1 MiB/s       | 28.2 MiB/s          |
| One open and close cycle | 23 ms           | 270 ms              |

Each difference broke a rule that came from one device:

- The class of the MTP interface. See `docs/05-finding-the-interface.md`.
- The wait before a storage appears. See `docs/01-cold-start.md`.
- The report about a cable. See `docs/04-file-transfer.md`.

A third device is more use to this project than a hundred more runs on these
two.

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
