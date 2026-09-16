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

| Crate       | Purpose                                     | State       |
| ----------- | ------------------------------------------- | ----------- |
| `ptp-proto` | PTP wire format. No I/O and no dependency.  | 17 tests pass |

## Tests

```
cargo test
```

The tests need no phone. The test data comes from a real capture of a Samsung
SM-S901U. `crates/ptp-proto/tests/fixtures/README.md` records the source.

## Test hardware

The project needs reports from many devices. At present the project has one:

| Device      | Android | Result                                  |
| ----------- | ------- | --------------------------------------- |
| Samsung SM-S901U | 16 | `simple-mtpfs` and `jmtpfs` both stop |

## Documentation language

The documentation uses Simplified Technical English (ASD-STE100). Many readers
of this project read English as a second language.

## Licence

BSD 2-Clause.
