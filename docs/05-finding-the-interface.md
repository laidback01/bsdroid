# How to find the MTP interface

A host must find the interface that carries MTP. The interface class is not
enough, and this document records why.

## Three makes of cellphone, three shapes

| Field              | Samsung SM-S901U | Motorola Moto G (5) | Cyrus CS 24 |
| ------------------ | ---------------- | ------------------- | ----------- |
| bInterfaceClass    | 0x06             | 0xff                | 0x06        |
| bInterfaceSubClass | 0x01             | 0xff                | 0x01        |
| bInterfaceProtocol | 0x01             | 0x00                | 0x01        |
| iInterface         | 5                | 6                   | 5           |
| The name of string | `MTP`            | `MTP`               | `MTP`       |
| Interfaces in all  | 4                | 2                   | 1           |
| An adb interface   | yes              | yes                 | no          |
| Endpoint, data in  | 0x81             | 0x81                | 0x81        |
| Endpoint, data out | 0x01             | 0x01                | 0x01        |
| Endpoint, event    | 0x82             | 0x82                | 0x82        |

Two devices use the still imaging class, which the USB standard defines for
this job. One device uses a vendor class.

The endpoints are the same on all three devices. The name is the same on all
three devices. The class is not.

### The name is the field that agrees

The class agrees on two devices of three. The name agrees on three of three,
across two chip makers.

The project does not yet search by name alone. Two devices give the standard
class, and the standard class needs no request to the device.

A fourth device with a third class changes this answer.

## The rule that failed

The project first looked for class 0x06, subclass 0x01, protocol 0x01. The rule
came from one cellphone.

The rule finds the Samsung. The rule does not find the Motorola, and the
Motorola gives MTP.

A host that uses the rule tells a Motorola user that the cellphone gives no MTP
interface. The cellphone does give one.

## The rule that works

1. Look for an interface with class 0x06, subclass 0x01, protocol 0x01. When
   one is there, take the interface.
2. Look for an interface with class 0xff, subclass 0xff, protocol 0x00, and a
   name. Read the name. When the name is `MTP`, take the interface.

The order matters for a reason. Step 1 reads the descriptor the host already
has. Step 2 needs a request to the device, and a request costs time.

`MtpInterface::find_with_names` holds the rule. The function takes a closure
that reads a name, because the pure module cannot make a request.

## A vendor class needs a name

A vendor class means "the standard does not define this". Many interfaces use
the class:

| Class | Subclass | Protocol | Job          |
| ----- | -------- | -------- | ------------ |
| 0xff  | 0xff     | 0x00     | MTP          |
| 0xff  | 0x42     | 0x01     | adb          |

The subclass and the protocol separate adb from MTP on the two test devices.
The name is the check that does not depend on a number that a vendor chooses.

A host must not take a vendor interface without the name. A test covers this:
the same Motorola descriptor, with a name of `Mass Storage`, gives no MTP
interface.

## A device with no adb interface

The Cyrus CS 24 gives one interface, and the interface carries MTP. The device
gives no adb interface, because the owner did not turn on USB debugging.

This matters for a fault message. The project tells an Android user that a
device is not in file transfer mode. The adb interface is how the project knows
that a device is Android.

A device with no adb interface, and no MTP interface, gives the host nothing to
recognise. The message is then a general one. The project has no better answer
for that case.

## What this means for a new device

The project has two devices, and the two disagree. A third device can disagree
again.

Report a device that this project does not find. `mtpprobe` gives the numbers
a report needs:

```
BSDROID_DEBUG=1 mtpprobe probe
```

The output holds one line for each interface, with the class, the subclass, the
protocol, the string index and the result of each test.
