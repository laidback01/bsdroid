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

## A name from a device is not a path

A device gives the name of an object. A name can hold a path separator, and a
name can be `..`.

The code takes the last part of the name, and the code rejects `.` and `..`.
A device therefore cannot make the host write outside the current folder.
