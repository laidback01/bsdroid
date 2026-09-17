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

## What the database is for

An earlier version of this document called the database a list of faults. That
is not right, and the source shows why.

`libusb1-glue.c` looks in the table first, by vendor identifier and product
identifier:

```
// First check if we know about the device already.
// Devices well known to us will not have their descriptors
// probed, it caused problems with some devices.
```

A device in the table is an MTP device, and `libmtp` reads no descriptor. A
device that is absent gets a second test: `libmtp` reads a Microsoft descriptor
at string index 0xee, and looks for the letters `MSFT`.

The second test needs a vendor class. The test for the still imaging class sits
inside `#if 0`, so the test never runs.

The table is therefore how `libmtp` finds most devices. The flags are a second
job of the same table.

### What this means for the comparison

This project reads the interface descriptor, and finds two shapes:

- the still imaging class, 0x06/0x01/0x01,
- a vendor class with the interface name `MTP`.

`libmtp` finds the first shape only from the table, because the test for that
class is off. This project finds the first shape for any device.

`libmtp` finds a vendor class device with a Microsoft descriptor. This project
does not read that descriptor, so this project misses such a device.

Neither project covers the other. The table holds many years of work, and a
descriptor test needs no entry for a new device.

## Does this project need the flags?

The database covers 1529 devices, and this project tested three. A fair
question follows: does this project work for a person with a different device?

### The count is smaller than it looks

Of the 1529 entries, 833 carry one flag group: `DEVICE_FLAGS_ANDROID_BUGS`.
The group holds six flags, and each of the 833 Android entries gets the same
six.

The database is therefore not 1529 different faults. For an Android device, the
database holds one rule, and the rule covers most entries. The other entries
cover a music player from an earlier time.

### The six flags, and what this project does

| The flag                              | What this project does            |
| ------------------------------------- | --------------------------------- |
| `DEVICE_FLAG_BROKEN_MTPGETOBJPROPLIST` | The project does not use the operation |
| `DEVICE_FLAG_BROKEN_SET_OBJECT_PROPLIST` | The project does not use the operation |
| `DEVICE_FLAG_BROKEN_SEND_OBJECT_PROPLIST` | The project does not use the operation |
| `DEVICE_FLAG_UNLOAD_DRIVER`           | The project takes the interface from the kernel, for each device |
| `DEVICE_FLAG_LONG_TIMEOUT`            | The deadline is 10 seconds, for each device |
| `DEVICE_FLAG_FORCE_RESET_ON_CLOSE`    | The project sends no reset, for each device |

Each row needs no device table. The project either avoids the operation, or
does the safe thing for every device.

The Samsung offset fault is the same. The workaround asks for one byte less at
the end of a file, and the workaround costs nothing on a device with no fault.
The project therefore applies the workaround to each device.

### Why the project does not link libmtp

Two reasons.

`libmtp` reaches a device through `libusb-1.0`. That layer holds the defect in
`docs/00-why.md`. A link to `libmtp` brings the defect back, and the defect is
the reason for this project.

`libmtp` is under the LGPL, version 2.1. This project is under the BSD licence,
with 2 clauses. A copy of the device table into this project needs the
agreement of many authors over many years.

### Where the risk really is

The risk is not a device quirk. The risk is the search for the interface.

Two of the three test devices use the standard USB class, and one uses a vendor
class with the name `MTP`. A fourth device can use a third shape, and this
project then finds nothing.

A report from a new device is worth more to this project than a table of
flags. `BSDROID_DEBUG=1 mtpprobe probe` prints what a report needs.

## What this means for the project

Read the database. The database holds the work of many people over many years,
and the work covers 1540 devices.

The database does not remove the need to measure. The database holds no entry
for 0x6866, and the database holds no timing for a modern cellphone. A
measurement adds to the database, and the database guards a measurement.
