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

Three readings of the same phone, with the same cable and the same host.

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

**Idea 1: a phone in charge mode removes the MTP interface.**

The idea gave a clean rule: no MTP interface means the wrong USB mode. Readings
B and C disprove the idea. The phone keeps the MTP interface in charge mode,
with the same number and the same endpoints.

**Idea 2: the interface count tells the host the USB mode.**

Reading B gives 2 interfaces and reading C gives 4. Both readings are charge
mode, and `sys.usb.config` gives the same value for both. The interface count
does not follow the USB mode, and a host must not read the mode from the count.

The project held idea 2 for a short time, because readings A and B agreed with
it. Reading C arrived later and broke it.

## What reading C settles

Reading C has an unlocked screen and charge mode, and the phone gives 0
storages. A locked screen is therefore not needed for the empty list. Charge
mode alone is enough.

Readings B and C differ in two ways at once, the screen lock and the interface
count. The two readings do not show which change caused the other. A test that
changes one thing at a time is still missing.

## An untested idea

Reading B is locked and gives 2 interfaces. Readings A and C are unlocked and
give 4. The pattern gives an idea: when the screen is unlocked, the phone gives
the full interface set.

Three readings are not enough to call this a rule. This document records the
idea as an idea.

The project holds a fixture for reading B. See
`crates/usb-freebsd/tests/fixtures/s22_charge_mode_descriptor.hex`.

## Image transfer mode works

The phone has a mode named "Transferring images". The mode carries PTP. MTP is
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

### A limit on this result

The test read the storage. The test did not list an object. PTP normally gives
the images, and MTP gives all the files. The capacity is the same in both
modes, and the set of objects is possibly not the same.

No test yet lists the objects. Do not tell a user that image mode gives the
same files as file transfer mode.

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
fields, on this phone. A test on one phone is not a rule for all phones.

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
developer mode on the phone, so the product cannot depend on `adb`.

No test yet shows a way to learn the USB mode over MTP alone. The
`GetDeviceInfo` operation gives a list of supported operations, and no test yet
shows that the list changes with the mode.
