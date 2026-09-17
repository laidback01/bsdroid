# How to find the MTP interface

A host must find the interface that carries MTP. The interface class is not
enough, and this document records why.

## Two makes of telephone, two shapes

| Field              | Samsung SM-S901U | Motorola Moto G (5) |
| ------------------ | ---------------- | ------------------- |
| bInterfaceClass    | 0x06             | 0xff                |
| bInterfaceSubClass | 0x01             | 0xff                |
| bInterfaceProtocol | 0x01             | 0x00                |
| iInterface         | 5                | 6                   |
| The name of string | `MTP`            | `MTP`               |
| Endpoint, data in  | 0x81             | 0x81                |
| Endpoint, data out | 0x01             | 0x01                |
| Endpoint, event    | 0x82             | 0x82                |

The Samsung uses the still imaging class, which the USB standard defines for
this job. The Motorola uses a vendor class, and gives the interface the name
`MTP`.

The endpoints are the same. Only the class differs.

## The rule that failed

The project first looked for class 0x06, subclass 0x01, protocol 0x01. The rule
came from one telephone.

The rule finds the Samsung. The rule does not find the Motorola, and the
Motorola gives MTP.

A host that uses the rule tells a Motorola user that the telephone gives no MTP
interface. The telephone does give one.

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
