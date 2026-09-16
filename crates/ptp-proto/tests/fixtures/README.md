# Test fixtures

These files hold real PTP traffic. A Samsung SM-S901U (Galaxy S22) produced the
traffic on FreeBSD 15.1-RELEASE-p2. The capture comes from `libmtp` with
`LIBMTP_DEBUG=9`.

The traffic is not synthetic. Do not edit the hex bytes. If a test disagrees
with a fixture, the test is wrong.

## Format

Each `.hex` file holds one PTP container. The parser for the files:

- ignores a blank line,
- ignores a line that starts with `#`,
- removes all whitespace from each other line,
- reads the remaining characters as hexadecimal bytes.

A `#` comment records what each field means. The comments make the wire format
readable in a diff.

## Cross-check

`mtp-detect` read the same device in a separate session, minutes before the
capture. The two tools agree on every constant value:

| Field              | Fixture bytes decode to | `mtp-detect` reports | Same? |
| ------------------ | ----------------------- | -------------------- | ----- |
| MaxCapacity        | 239935107072            | 239935107072         | yes   |
| FreeSpaceInObjects | 1073741824              | 1073741824           | yes   |
| StorageDescription | `Internal storage`      | `Internal storage`   | yes   |
| FreeSpaceInBytes   | 204712546304            | 204718743552         | no    |

The agreement on the constant values makes the fixture a trustworthy
reference.

## Do not cross-check a value that changes

FreeSpaceInBytes differs by 6197248 bytes, which is 5.91 MiB. The difference is
not a parser fault. The phone wrote log and cache files between the two runs.

The first version of the test used the `mtp-detect` value for this field, and
the test failed. The lesson: a fixture is a record of one moment. Compare a
constant value across sessions. Do not compare a value that changes.

The tests now expect the value the captured bytes hold.
