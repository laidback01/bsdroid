# What libmtp already knew

`libmtp` holds a database of device faults. The file `src/music-players.h`
holds 1540 entries, and each entry names a device and the faults of the device.

This project measured three devices with no knowledge of that database. This
document compares the two.

## All three test devices are in the database

| Device            | Identifier    | The name in the database        |
| ----------------- | ------------- | ------------------------------- |
| Samsung SM-S901U  | 04e8:6860     | `Galaxy models (MTP)`           |
| Motorola Moto G 5 | 22b8:2e76     | `Moto E/G (ID1) (MTP+ADB)`      |
| Cyrus CS 24       | 0e8d:2008     | `MT65xx/67xx (MTP mode)`        |

The Cyrus is not in the database by name. The database holds the chip, and not
the cellphone. Many makers sell a MediaTek chip under a new name, so the chip
is the useful entry.

### One cellphone, three names

The third test device carries three names:

| The source of the name | The name                          |
| ---------------------- | --------------------------------- |
| The shop               | NUU B20                           |
| The USB descriptor     | `Cyrus Technology CS 24`          |
| The `libmtp` database  | `MediaTek MT65xx/67xx (MTP mode)` |

The owner bought a NUU B20. The descriptor names a different company, because
another company built the cellphone. The database names the chip, because many
cellphones hold the same chip.

A host must therefore identify a device by `idVendor` and `idProduct`. A name
is for a person to read.

The project met this fault a second time. FreeBSD names the Samsung
`GT-I9070 (network tethering)` in tethering mode, and a GT-I9070 is a cellphone
from 2012. See `docs/02-device-states.md`.

## Where the measurements agree

| What this project measured        | The database                        |
| --------------------------------- | ----------------------------------- |
| 0x6860 is MTP mode                | `0x6860 - MTP mode (default)`       |
| 0x6864 is tethering, and not MTP  | `0x6864 - USB CDC RNDIS ADB`        |
| 0x686c is MIDI                    | `0x686c - MIDI ADB mode`            |
| 0x2e76 is the Motorola in MTP mode | `Moto E/G (ID1) (MTP+ADB)`         |
| 0x2008 is the Cyrus in MTP mode   | `MT65xx/67xx (MTP mode)`            |
| `GetObjectPropList` works on 0x6860 | the fault flag is commented out   |

Six measurements of six agree.

## Where this project found something new

The Samsung gives 0x6866 in image transfer mode. The database holds 0x6865 for
that mode, and holds no 0x6866.

The database entry comes from older cellphones. The SM-S901U is newer, and the
identifier changed.

This is a small contribution to the database.

## What the database knew, and this project did not

### The Samsung offset fault

The comment in `src/device-flags.h` starts with these words:

```
The MTP stack of Samsung Galaxy devices has a mysterious bug in
GetPartialObject.
```

The rest of the comment gives the condition. This document gives the condition
in short sentences, and `src/device-flags.h` holds the words of the author.

The condition:

1. The host asks for the last bytes of a file.
2. The size of the answer makes the last USB packet exactly the packet size of
   USB 2.0, which is 512 bytes.

The device then stops, and the host reports a timeout.

The flag `DEVICE_FLAG_SAMSUNG_OFFSET_BUG` marks the fault. The flag is on the
entry for 0x6860, which is a test device of this project.

`GetPartialObject` is the operation the filesystem design chose for a read. The
fault therefore matters. See `docs/07-filesystem-design.md`.

This project did not find the fault, because this project did not read the end
of a file with a partial read.

### Other knowledge in the database

| Flag                       | What the flag says                          |
| -------------------------- | ------------------------------------------- |
| `DEVICE_FLAG_LONG_TIMEOUT` | The Samsung needs a long deadline           |
| `DEVICE_FLAG_UNLOAD_DRIVER` | The host must take the interface from the kernel |
| `DEVICE_FLAGS_ANDROID_BUGS` | A group of faults for each Android device  |

A comment on the Samsung entry holds two more facts:

- The session must open about 3 seconds after a connect, or the device stops
  answering.
- A read of exactly 512 bytes, which is the USB 2.0 packet size, hangs the
  device.

## Where this project confirmed the database, with no knowledge of it

The flag `DEVICE_FLAG_FORCE_RESET_ON_CLOSE` holds this comment:

```
This flag indicates that the device need an explicit
USB reset after each connection. Some devices don't
like this, so it's not done by default.
```

This project sent a reset at each open. A Cyrus CS 24 then failed 4 of the 5
test runs. The project removed the reset, and the device passed each of the 6
test runs. See `docs/06-the-reset-that-breaks.md`.

The database says the same thing in one sentence: some devices do not like a
reset, so a reset is not the default.

The two answers agree, and the two answers come from different work.

## What this means for the project

Read the database. The database holds the work of many people over many years,
and the work covers 1540 devices.

The database does not remove the need to measure. The database holds no entry
for 0x6866, and the database holds no timing for a modern cellphone. A
measurement adds to the database, and the database guards a measurement.
