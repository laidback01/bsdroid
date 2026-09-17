# Mount the cellphone when a person connects the cellphone

A person connects a cellphone, and the folder appears. This document says how,
and says why one of the two FreeBSD mechanisms does not fit.

## autofs does not fit

FreeBSD holds `autofs`, and `autofs` mounts NFS well. It does not fit MTP, for
two reasons.

The first reason is the trigger. A program reads the path, and `autofs` then
mounts a filesystem. A person who connects a cellphone gets nothing until that
person reads `/media/phone`. The request here is the other way: the cellphone
arrives, and the folder appears.

The second reason is the mount itself. `automountd` runs `mount -t <kind>`, and
`mount` then runs `/sbin/mount_<kind>`. A FUSE filesystem mounts itself, and
this project gives no `mount_mtpfs`, so `automountd` has no program to call.

## devd fits

`devd` reads each event of the kernel and runs a command for an event that
matches. A USB attach is such an event. `devd` is the FreeBSD answer to a
`udev` rule.

The event of a Samsung on this host:

```
!system=USB subsystem=DEVICE type=ATTACH ugen=ugen0.11 cdev=ugen0.11
vendor=0x04e8 product=0x6860 devclass=0x00 devsubclass=0x00
sernum="R5CRC40FJKT" release=0x0504 mode=host port=2 parent=ugen0.1
```

Two files do the work:

| File                                   | What it holds        |
| -------------------------------------- | -------------------- |
| `tools/automount/bsdroid.conf`         | The rules for `devd` |
| `tools/automount/bsdroid-automount`    | The helper           |

## The rule matches the vendor, and not the product

An Android cellphone gives a different product for each USB mode. The table in
`docs/02-device-states.md` holds 6860, 6864, 6866 and 686c for one Samsung.

A rule for one product therefore misses the cellphone in three modes out of
four. The rule matches the vendor alone, and the helper looks for MTP. A
cellphone that gives no MTP costs one failed probe, and no message.

## The mount runs as the person, and not as root

`devd` runs as root. The helper does not keep that privilege.

A mount that root makes is a mount that root owns, and the desktop then needs
`allow_other` to reach the files. The helper instead runs `mtpfs` as the
account of the person, with `su -m`. Two things make that possible:

| Need                        | Why                          |
| --------------------------- | ---------------------------- |
| `vfs.usermount=1`           | A person can mount           |
| Membership of `operator`    | Read and write on the `ugen` device |

The account must also own the mount point, because `vfs.usermount` gives a
mount to the owner of the folder alone. The helper makes the folder and gives
the folder to that account.

`mtpfs` reports the user that runs the mount as the owner of each object. MTP
holds no owner, and an earlier version reported user 0. `ls -l` then showed
each file as a file of root, on a mount that the person can write. The report
of the true owner removes that difference.

## A cellphone gives more than one attach event

The measured order for a Samsung, from `/var/run/devd.pipe`:

```
type=ATTACH  vendor=0x04e8 product=0x6860
type=DETACH  vendor=0x04e8 product=0x6860
type=ATTACH  vendor=0x04e8 product=0x6860
```

A person selects the USB mode, and the cellphone then joins the bus again. Two
helpers therefore run, and both start a mount of one folder.

`lockf` gives one helper at a time for one mount point. The lock waits, and
does not give up at once, because the second event is the event that carries
the cellphone the person wants. A helper that drops the second event drops the
mount.

A helper that holds the lock for the whole deadline is the other fault. The
wait therefore ends as soon as the `ugen` node goes away, and the next helper
gets the lock.

The check of two helpers at one time:

| Measurement                  | Result |
| ---------------------------- | ------ |
| Mount entries for the folder | 1      |
| `mtpfs` processes            | 1      |

The second helper reports `already holds a mount` and stops.

## The name of the mount point comes from the cellphone

The first version named the folder `/media/mtp-04e8`, from the USB vendor. The
name is stable, and a person cannot read it.

A cellphone gives two names, and they are not the same name:

| Source          | Command            | Answer                    |
| --------------- | ------------------ | ------------------------- |
| USB descriptor  | `mtpfs -l`         | `SAMSUNG SAMSUNG_Android` |
| MTP `DeviceInfo`| `mtpfs --name`     | `samsung SM-S901U`        |

The model is the better name for a person, so `mtpfs --name` gives it and the
helper makes `/media/samsung-sm-s901u`.

The name of the model does not change with the USB mode. The folder therefore
keeps one name, and a bookmark in a file manager keeps working. A name from the
product identifier does change. A cellphone gives a different product for each
mode.

`mtpfs --name` does one more job. The command fails for a cellphone that gives
no MTP, so the answer also says whether the cellphone is in file transfer
mode. The helper therefore makes no folder for a cellphone in charge mode.

## One session at a time, and the wait for the last one

MTP gives one session at a time. A program that closes a session leaves the
device busy for a moment.

The measurement on a Samsung, with no wait between the two commands:

| When                     | What `OpenSession` gave |
| ------------------------ | ----------------------- |
| at once after an unmount | timed out               |
| one second later         | the session             |

The helper met this each time a cellphone joined the bus twice. `Mtp::open`
therefore tries again for an error of the USB layer, and for an error of the
session layer. The patience is the patience that the search for a device
uses.

```
mtpfs: attempt 1 gave: OpenSession: the transfer on endpoint 0x81 gave: timed out
mtpfs: the session opened on attempt 2
```

Three answers do not change: a fault of the device, a name that does not
parse, and a device that lacks an operation. Those three come back at once.

## A test of a mount must run as the owner of the mount

The cellphone is gone by the time the detach event arrives. The name of the
mount point therefore cannot come from the cellphone. The helper writes the
name to a file in `/var/run` at attach time, with the serial number as the key.
devd gives the serial number for both events.

A state file that is not there leaves one way: look at each mount, and find
the mount that answers nothing.

That test ran as root in the first version, and it unmounted a cellphone that
was working. FUSE gives a mount to the account that made the mount, and that
mount had no `allow_other`, so root reached none of it. The test read
"root cannot list this folder" as "this cellphone is gone".

The `mount` table names the account:

```
mtpfs on /media/samsung-sm-s901u (fusefs, nosuid, mounted by jax)
```

The test now runs as that account. A live mount stays, and a dead mount goes.

## The helper does not keep devd busy

`devd` waits for the command of a rule. The helper waits up to 25 seconds for
the mount, so the helper cannot be that command.

`daemon -f` puts the work in another process, and the command returns at once.

## Install

```
doas cp tools/automount/bsdroid.conf /usr/local/etc/devd/
doas cp tools/automount/bsdroid-automount /usr/local/sbin/
doas cp tools/automount/bsdroid-automount.conf.sample \
    /usr/local/etc/bsdroid-automount.conf
doas service devd restart
```

Change the vendor in `bsdroid.conf` for a cellphone that is not a Samsung. Run
`mtpfs -l` for the vendor.

Name the account in `/usr/local/etc/bsdroid-automount.conf`:

```
MOUNT_USER="jax"
```

An empty name makes the helper guess the account. Each guess can be wrong. The
`who` table on this host holds no login, because a display manager starts the
desktop, so the first guess gives nothing here.

## Read the messages

```
tail -f /var/log/messages | grep bsdroid
```

The helper writes at `daemon.notice`. An earlier version wrote at
`daemon.info`, and `/var/log/messages` holds `*.notice` and above, so every
message went nowhere.

## What a person sees

```
Sep 17 13:18:51 hpz6 bsdroid-automount[27071]: mount samsung SM-S901U on /media/samsung-sm-s901u for jax
Sep 17 13:18:51 hpz6 bsdroid-automount[27082]: /media/samsung-sm-s901u is ready
```

```
mtpfs on /media/samsung-sm-s901u (fusefs, nosuid, mounted by jax)
drwxr-xr-x  2 jax jax 0 Dec 31  1969 DCIM
mtpfs  223G  33G  191G  15%  /media/samsung-sm-s901u
```

A read, a write, an append and an edit in place all work through that path, as
the account of the person, with no root.
