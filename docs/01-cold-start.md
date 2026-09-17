# The cold start

An Android device reports no storage for a short time after a connect. The
device answers correctly, and the answer is an empty list. A program that asks
one time reports "no files", and the report is wrong.

This document records the measurement.

## The symptom

`mtpprobe` opened the USB device 20 times in a row. Each cycle read the storage
list one time:

```
  cycle   1/20       24 ms   0 storage(s)
  cycle   2/20       22 ms   1 storage(s)
  cycle   3/20       23 ms   1 storage(s)
  ...
  cycle  20/20       20 ms   1 storage(s)
```

Cycle 1 read no storage. Cycle 2 read one storage. Nobody touched the phone
between the two cycles.

A second run of the same test, a few seconds later, read one storage on every
cycle. The state is therefore a property of the connect, not of the program.

## The measurement

The `coldstart` command resets the USB device and measures the wait. A reset
puts the device in the state that follows a connect, and a reset needs no
person to pull the cable.

Test system:

- FreeBSD 15.1-RELEASE-p2, amd64
- Samsung SM-S901U, Android 16, file transfer mode

Result:

| Event                             | Time    |
| --------------------------------- | ------- |
| The device left the bus           | 3624 ms |
| The device came back              | 4167 ms |
| A storage appeared, on attempt 2  | +307 ms |

The first `GetStorageIDs` after the reset gave response code 0x2001 (OK) and an
empty list. The second gave one storage.

## The cold start is not a rule for every device

A Motorola Moto G (5) does not do this. The same test, on the same host:

| Device           | Attempts until a storage appeared |
| ---------------- | --------------------------------- |
| Samsung SM-S901U | 2                                 |
| Motorola Moto G (5) | 1                              |

The Motorola answered the first `GetStorageIDs` with a storage, each time.

An earlier version of this document called the cold start a property of
Android. Two devices now disagree, so the cold start is a property of a device,
and not of Android.

The retry costs nothing on a device that answers at once. A host must therefore
still retry, because a host does not know which device it holds.

## Why the empty list is not an error

The MTP service on the device starts after the USB interface starts. The device
can answer a command before the service knows the storage. The empty list is
the correct answer at that moment.

The device does not report a fault, because there is no fault. The host asked a
question too early.

## What a program must do

Ask again. The rule:

1. Send `GetStorageIDs`.
2. If the list holds a storage, continue.
3. If the list is empty, wait, and go to step 1.
4. Stop after a count limit.

`mtpprobe` makes 10 attempts, with 300 milliseconds between two attempts. The
limit keeps rule 1 in `docs/00-why.md`: a loop must not depend on the device for
the end condition.

## What this means for a user report

A user who says "the tool shows no files" can have a device that works. The
correct question is not "are the files there". The correct question is "how many
times did the tool ask".

`mtpprobe` prints the attempt count for this reason:

```
    GetStorageIDs       307 ms   2 attempt(s)   1 storage(s)
      the first attempt gave an empty list, and the host retried
```

## A mistake this finding corrected

The first version of `mtpprobe` asked one time. The program then reported 0
storages and gave three causes:

- a locked screen,
- a USB mode that is not file transfer,
- a permission question with no answer.

All three causes are real. None of the three was the cause on the test system.
The message sent the user to the phone, and the phone was correct.

A diagnostic tool that names the wrong cause is worse than a tool that names no
cause. The tool now retries first, and the tool names the retry count.
