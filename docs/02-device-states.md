# What a host can learn, and what a host cannot

A host asks two questions about an Android device:

1. Does the device have an MTP interface?
2. Does the device give file access?

The two questions have different answers. This document records the
measurements that show the difference.

## The finding

A Samsung SM-S901U keeps the MTP interface in charge mode. The interface has
the same number and the same endpoints in both modes.

A host that finds an MTP interface does not know that the host can read a file.

## The measurements

Three readings of the same cellphone, with the same cable and the same host.

| Item                | A: file transfer | B: charge, locked | C: charge, unlocked |
| ------------------- | ---------------- | ----------------- | ------------------- |
| `sys.usb.config`    | `mtp,adb`        | `sec_charging,adb` | `sec_charging,adb` |
| Screen              | unlocked         | locked            | unlocked            |
| Interfaces          | 4                | 2                 | 4                   |
| Descriptor size     | 136 bytes        | 70 bytes          | 136 bytes           |
| MTP interface       | 0                | 0                 | 0                   |
| MTP bulk endpoints  | 0x81, 0x01       | 0x81, 0x01        | 0x81, 0x01          |
| adb interface       | 3                | 1                 | 3                   |
| `OpenSession`       | OK               | OK                | OK                  |
| Storages            | 1                | 0                 | 0                   |

## Two ideas, and what the readings did to them

**Idea 1: a cellphone in charge mode removes the MTP interface.**

The idea gave a clean rule: no MTP interface means the wrong USB mode. Readings
B and C disprove the idea. The cellphone keeps the MTP interface in charge mode,
with the same number and the same endpoints.

**Idea 2: the interface count tells the host the USB mode.**

Reading B gives 2 interfaces and reading C gives 4. Both readings are charge
mode, and `sys.usb.config` gives the same value for both. The interface count
does not follow the USB mode, and a host must not read the mode from the count.

The project held idea 2 for a short time, because readings A and B agreed with
it. Reading C arrived later and broke it.

## What reading C settles

Reading C has an unlocked screen and charge mode, and the cellphone gives 0
storages. A locked screen is therefore not needed for the empty list. Charge
mode alone is enough.

Readings B and C differ in two ways at once, the screen lock and the interface
count. The two readings do not show which change caused the other. A test that
changes one thing at a time is still missing.

## An untested idea

Reading B is locked and gives 2 interfaces. Readings A and C are unlocked and
give 4. The pattern gives an idea: when the screen is unlocked, the cellphone
gives
the full interface set.

Three readings are not enough to call this a rule. This document records the
idea as an idea.

The project holds a fixture for reading B. See
`crates/usb-freebsd/tests/fixtures/s22_charge_mode_descriptor.hex`.

## Image transfer mode works

The cellphone has a mode named "Transferring images". The mode carries PTP. MTP
is
an extension of PTP, so the two modes share the interface class.

Reading D, with the screen unlocked:

| Item             | Value                           |
| ---------------- | ------------------------------- |
| `sys.usb.config` | `ptp,adb`                       |
| `idProduct`      | 0x6866                          |
| Interfaces       | 2                               |
| MTP interface    | 0, endpoints 0x81, 0x01, 0x82   |
| `OpenSession`    | 0x2001 OK                       |
| `GetStorageIDs`  | 0x2001 OK, 1 storage, attempt 2 |
| `GetStorageInfo` | 0x2001 OK, `Internal storage`   |

The host read the storage, the capacity and the free space. `mtpprobe` needed
no change for this mode.

The first attempt gave an empty list, and the second attempt gave the storage.
The cold start in `docs/01-cold-start.md` happens in this mode too. A host that
asks one time reports "no files" for a mode that works.

### A limit on this result, which a later test measured

The test above read the storage, and the test did not list an object.

A later test listed the objects. Image transfer mode gives 821 objects, and
file transfer mode gives 2059 objects on the same cellphone. Image transfer mode
gives 2 folders in the root, and file transfer mode gives 13.

The capacity and the free space are the same in both modes, so the storage test
cannot see the difference. See `docs/03-object-handles.md`.

Image transfer mode is a fallback for a photograph. Image transfer mode is not
a fallback for a file.

## Two fields separate file transfer from image mode

The class, the subclass and the protocol are the same in both modes. Two other
fields are not:

| Field                      | File transfer | Image transfer |
| -------------------------- | ------------- | -------------- |
| `idProduct`                | 0x6860        | 0x6866         |
| `iInterface` of interface 0 | 5             | 0              |

`idProduct` lives in the device descriptor. `iInterface` lives in the
configuration descriptor, and `Interface::string_index` holds the value.

This finding narrows an earlier statement in this document. A host cannot learn
the USB mode from the interface class. A host can learn something from other
fields, on this cellphone. A test on one cellphone is not a rule for all phones.

## Tethering mode removes the interface

Reading E is USB tethering, with the screen unlocked:

| Item             | Value                                     |
| ---------------- | ----------------------------------------- |
| `sys.usb.config` | `rndis,adb`                               |
| `idProduct`      | 0x6864                                    |
| Interfaces       | 3: RNDIS 0xe0, CDC data 0x0a, adb 0xff    |
| MTP interface    | absent                                    |

`mtpprobe` reported the correct cause:

```
RESULT: an Android device is connected, and the device gives no MTP
  interface. The device shows the adb interface, so the device is
  awake and the cable carries data.
```

This reading is the first test of that message against real hardware.

### The trap in this capture

The CDC data interface owns endpoint 0x81 and endpoint 0x01. The same two
addresses carry MTP in file transfer mode and in image mode.

A host that looks for a bulk endpoint pair, and does not check the interface
class first, opens the network interface. The host then sends PTP to a network
device. The class check is the reason this project does not do that.

### The product string is wrong

FreeBSD names the device in this mode:

```
GT-I9070 (network tethering, USB debugging enabled)
```

A GT-I9070 is a Galaxy S Advance from 2012. The cellphone is an SM-S901U from
2022.
The name comes from a product identifier table in the host, and the table maps
0x6864 to the old model.

Do not use the product string to identify a device. Use `idVendor` and
`idProduct`, and treat the string as a label for a person to read.

## MIDI mode, and a second trap

Reading F is MIDI mode, with the screen unlocked:

| Item             | Value                                       |
| ---------------- | ------------------------------------------- |
| `sys.usb.config` | `midi,adb`                                  |
| `idProduct`      | 0x686c                                      |
| Interfaces       | 3: audio control, MIDI streaming, adb       |
| MTP interface    | absent                                      |

The audio class uses 9 bytes for an endpoint descriptor. The standard endpoint
descriptor holds 7 bytes.

A parser that adds 7 to the position reads the next descriptor at the wrong
offset. Every descriptor after the first endpoint is then wrong. A parser must
add the length byte of each descriptor.

The project parser adds the length byte, because a length of 0 must give an
error. The rule came from the endless loop in `docs/00-why.md`. The rule also
solves this problem, and no test covered a descriptor of 9 bytes until this
capture.

MIDI mode also gives endpoint 0x01 and endpoint 0x81 to another interface, as
tethering mode does. Two modes now hold that trap, with two different interface
classes.

### A note about the passcode

The user reported that MIDI mode needed no passcode on the cellphone, and that
file
transfer mode did need one. The project made no measurement of this behaviour.
The note is a report from a person, and not a test result.

MIDI mode gives no file access, so the difference does not put a file at risk
here.

## One product identifier for most modes

| Mode                 | `sys.usb.config`   | `idProduct` | MTP interface |
| -------------------- | ------------------ | ----------- | ------------- |
| File transfer        | `mtp,adb`          | 0x6860      | present       |
| Charge only          | `sec_charging,adb` | 0x6860      | present       |
| Transferring images  | `ptp,adb`          | 0x6866      | present       |
| USB tethering        | `rndis,adb`        | 0x6864      | absent        |
| MIDI                 | `midi,adb`         | 0x686c      | absent        |

Charge mode and file transfer mode share `idProduct`. The two modes therefore
need the storage test, and no descriptor field separates them.

## Three devices in charge mode

A device in charge mode gives no file access. The descriptor is a different
question, and the three test devices do not agree:

| Device              | MTP interface in charge mode | The descriptor differs |
| ------------------- | ---------------------------- | ---------------------- |
| Samsung SM-S901U    | present                      | no, `idProduct` is the same |
| Motorola Moto G (5) | absent                       | yes                    |
| Cyrus CS 24         | present                      | no, and the bytes are the same |

The Cyrus gives the same 39 bytes in file transfer mode and in charge mode:

```
09 02 27 00 01 01 04 80 fa 09 04 00 00 03 06 01 01 05
07 05 81 02 00 02 00 07 05 01 02 00 02 00 07 05 82 03 1c 00 06
```

The `idProduct` is 0x2008 in both modes. The name of the interface is `MTP` in
both modes. Nothing in the descriptor separates the two modes.

### The rule

Two devices of three keep the MTP interface in charge mode. One of the two
gives a descriptor that does not change at all.

A host therefore cannot read the mode from the descriptor. A host must ask for
the storage, and read the answer.

`mtpprobe` says this to a user, and two makes of cellphone now support the
statement.

## The name of a mode is not the same on each cellphone

| Mode      | Samsung SM-S901U                    | Cyrus CS 24        | Motorola Moto G (5)       |
| --------- | ----------------------------------- | ------------------ | ------------------------- |
| MTP       | `Transferring Files / Android Auto` | `File Transfer`    | `Transfer files`          |
| Tethering | `USB tethering`                     | `USB Tethering`    | absent                    |
| MIDI      | `MIDI`                              | `MIDI`             | `Use this device as MIDI` |
| PTP       | `Transferring Images`               | `PTP`              | `Transfer photos (PTP)`   |
| No data   | `Charging phone only`               | `No data transfer` | `Charge this device`      |

Every mode charges the cellphone. The name of a mode describes the data, and
not the power.

### No name agrees on all three devices

Not one row above holds the same name three times. The name `MIDI` agrees on
two devices, and the third device writes `Use this device as MIDI`.

The Motorola gives no option for tethering in this menu. A cellphone holds that
setting in another place.

### A word is better help than a name

A list of names fails on the next cellphone. A word in the name does not:

| Mode    | The word that each name holds |
| ------- | ----------------------------- |
| MTP     | `file`                        |
| PTP     | `photo`, `image` or `PTP`     |
| No data | `charge`, or `no data`        |

Each of the three names for MTP holds the word `file`. `mtpprobe` therefore
tells a user to look for the word, and gives the three names as examples.

An earlier version of this document gave `File transfer` and `Charging only`
for the Samsung. Both names were wrong. The tool then told a user to choose an
option that the cellphone does not hold, which is worse than no help at all.

## What this means for a fault report

The host can state these facts:

- The device has an MTP interface.
- The device answered `OpenSession` with OK.
- The device answered `GetStorageIDs` with OK and an empty list, `n` times.

The host cannot state the cause. These three device states give the same
answer:

1. The USB mode is charge, and not file transfer.
2. The screen is locked.
3. The MTP service is still starting.

`mtpprobe` separates state 3 from the other two, because `mtpprobe` asks again.
A device in state 3 gives a storage on a later attempt. See
`docs/01-cold-start.md`.

`mtpprobe` cannot separate state 1 from state 2. The tool says so, and the tool
gives the user two steps in place of one cause.

## The rule for a message

Report the evidence first, and the cause second. A user who reads "the device
answered OK and gave 0 storages 10 times over 2765 ms" learns something true. A
user who reads "your screen is locked" learns something the host does not know.

An earlier version of `mtpprobe` broke this rule. The tool named a locked screen
as the cause, and the screen was not locked. `docs/01-cold-start.md` records
that mistake.

## An open question

A host with `adb` can read `sys.usb.config` and learn the mode. `adb` needs
developer mode on the cellphone, so the product cannot depend on `adb`.

No test yet shows a way to learn the USB mode over MTP alone. The
`GetDeviceInfo` operation gives a list of supported operations, and no test yet
shows that the list changes with the mode.
