# The file copy

`GetObject` copies one object from the device to the host. The operation is the
first one that moves a large payload, and the first one that writes to a disk.

## The host does not hold the file in memory

`Session::operation` keeps the payload in a vector. A vector is correct for a
small answer, such as the 62 bytes of `StorageInfo`. A vector is wrong for a
video of 10 GB.

`Session::operation_stream` writes each transfer to a writer, and the host
keeps only one read buffer. `Session::operation` calls
`Session::operation_stream` with a vector, so the two functions share one path
and one set of checks.

## The measurement

A Samsung SM-S901U in image transfer mode, on a host with FreeBSD
15.1-RELEASE-p2. The phone connects through a chain of hubs, and the link runs
at high speed, which is 480 Mbit each second.

| File size | Read size | Reads | Time   | Rate         |
| --------- | --------- | ----- | ------ | ------------ |
| 290053    | 64 KiB    | 5     | 21 ms  | 12.7 MiB/s   |
| 8142310   | 64 KiB    | 125   | 241 ms | 32.1 MiB/s   |
| 8142310   | 4 KiB     | 1988  | 311 ms | 25.0 MiB/s   |

A small file gives a low rate, because the operation cost does not change with
the size.

## The check

A size that agrees is not a proof. Two files of the same size hold different
bytes.

The host copied each file, and then compared a SHA-256 sum. The phone made the
second sum, with the `sha256sum` command over `adb`.

| File                        | Host sum | Phone sum |
| --------------------------- | -------- | --------- |
| IMG_20260118_074042.jpg      | 205740b4… | 205740b4… |
| c3d48750-…-9e45ab2dbf5d.jpg | 25dc6d0f… | 25dc6d0f… |

The sums agree. The transport, the container parser, the join of the transfers
and the write to disk are all correct.

The second file needed 1988 reads. A fault in the join of the transfers gives a
different sum, so the test covers the join.

## The read size changes the rate, and not the result

A read of 4 KiB needs 1988 reads, and a read of 64 KiB needs 125. The two runs
give the same SHA-256 sum.

The small read costs 22% more time. The cost is low, because each read has a
small fixed cost and the device sends the same bytes.

The environment variable `BSDROID_READ_BUFFER` sets the read size. The variable
is a test hook, and not a setting for a user.

## What limits the rate

The first measurement gave 32.1 MiB each second. The question is what sets the
limit: the code, the phone, the cable or the port.

The project tested each one.

| Test                                  | Result                          | What the result rules out |
| ------------------------------------- | ------------------------------- | ------------------------- |
| Phone through a chain of hubs         | HIGH speed, 32.1 MiB/s          | nothing yet               |
| Phone direct to a port on the board   | HIGH speed, no change           | the hub chain             |
| Second cable, heavier shielding       | HIGH speed, no change           | one bad cable             |
| BOS descriptor of the phone           | the phone supports super speed  | the phone                 |
| Flash drive, back panel               | SUPER speed, 5 Gbit each second | the host and the driver   |
| Flash drive, the port the phone used  | SUPER speed, 5 Gbit each second | that port, and the hubs   |

The last test is the one that closes the question. A flash drive needs no
cable, and the drive reached super speed in the same port that gave the phone
high speed.

Every part of the path can do super speed:

- the phone, by the BOS descriptor,
- the port, by the flash drive,
- the hubs, because the port sits behind them,
- the host and the driver, by the 400 MB each second the drive reached.

The cable is the only part left, and two cables gave the same result. Both
cables carry USB 2.0.

### A claim this test corrected

An earlier note in this project said the chain of hubs set the limit. The note
came from one measurement, and the note named a cause with no test.

The flash drive reached super speed through the same chain of hubs. The hubs
were never the limit.

### The BOS descriptor is the useful test

A host cannot learn the ability of a device from the speed of the link. A
device that supports super speed, on a cable with no super speed wires, reports
high speed.

The BOS descriptor holds the answer, and the answer does not change with the
cable:

```
0a 10 03 00 0f 00 01 0a ff 01
      ^^          ^^^^^
      |           wSpeedsSupported = 0x000f
      bDevCapabilityType = 0x03, super speed
```

`wSpeedsSupported` holds one bit for each speed. Bit 3 means super speed, and
the phone sets the bit.

`mtpprobe bench` reads this descriptor. A user therefore learns whether a
better cable helps, before the user buys a cable.

### A cable for a telephone is often USB 2.0

A cable that carries only USB 2.0 needs 4 wires. A cable that carries super
speed needs 9. The plug looks the same.

Two cables gave high speed on the test system. A cable that came with a disk is
a better test than a cable that came with a telephone.

## The link runs at 480 Mbit each second

A USB link at high speed carries 480 Mbit each second, which is 60 MB each
second. A real link reaches about 40 MB each second, because the protocol needs
part of the time.

The measurement of 32.1 MiB each second is near that limit. The limit is the
USB link, and not the code.

An SM-S901U supports a faster USB mode. The test host connects the phone
through a chain of hubs, and the chain gives high speed. A direct cable gives a
faster link.

Do not compare a rate from this document with a rate from a different host.

## A stopped transfer can hold a device

A benchmark found a fault in this project. The host stopped a data phase in the
middle, and the device then answered no command.

### The fault

The code had a limit of 8192 reads for one data phase. A read of 4 KiB
therefore stopped at 33554432 bytes, which is 32 MiB. A video of 39 MB failed,
and the failure message named the count:

```
GetObject: the device sent 33558528 bytes of the 39836906 it declared
```

The limit came from a number somebody chose. The limit now comes from the other
limits, so the limit cannot stop a transfer that the device can finish.

### What the fault did to the device

The device still held 6281974 bytes for the host. The host then sent a new
command, and the write to the device did not finish. Every later command
failed.

A user sees a phone that works with no program. The cause is the earlier
program, and not the phone.

### What repairs the device

The project tried four repairs, in this order:

| Repair                            | Result                                |
| --------------------------------- | ------------------------------------- |
| Read and drop the rest             | dropped 6281974 bytes                 |
| Clear the halt on both endpoints   | the endpoints work again              |
| Device reset request, 0x66         | the device answers a command again    |
| `CloseSession` and `OpenSession`   | the device still reports a session     |

The device reset request is the important one. The still imaging class defines
request 0x66, and the request needs no root. Before the request, every command
reached the deadline. After the request, a command took 5 milliseconds.

The repairs run at the start of each session, so a user does not meet the fault
of an earlier program.

### What the repairs do not fix

The device kept one session open, and the device refused to close the session.
A user then needs to disconnect the cable and connect the cable again.

A program must therefore not stop in the middle of a data phase. The repairs
reduce the damage, and the repairs do not remove the damage.

## A name from a device is not a path

A device gives the name of an object. A name can hold a path separator, and a
name can be `..`.

The code takes the last part of the name, and the code rejects `.` and `..`.
A device therefore cannot make the host write outside the current folder.
