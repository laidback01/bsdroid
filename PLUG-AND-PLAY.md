# Plug and play

This document is the goal of the project, in one page.

Connect the cellphone. Unlock the cellphone. Select file transfer mode. A
folder appears, and your file manager opens the folder. Pull the cable, and the
folder goes away.

No daemon. No server. No account. No network. Nothing installed on the
cellphone. No root.

Every other document in `docs/` holds the measurement for one part. This
document says how to get the whole of it, and what to watch for.

## What you get

```
$ mount | grep mtpfs
mtpfs on /media/samsung-sm-s901u (fusefs, nosuid, mounted by jax)

$ ls /media/samsung-sm-s901u
Alarms      Audiobooks  Documents  Movies  Notifications  Podcasts  Ringtones
Android     DCIM        Download   Music   Pictures       Recordings
```

The folder holds the name of the model of the cellphone. The name does not
change, so a bookmark in a file manager keeps working.

You can read a file, write a file, make a folder, remove a file, rename a
file, and change a file in place. You own each file, because the mount runs as
your account.

## What you need

| Need                        | How to check                    |
| --------------------------- | ------------------------------- |
| FreeBSD 15.1 or later       | `freebsd-version`               |
| `fusefs-libs3`              | `pkg info fusefs-libs3`         |
| `vfs.usermount=1`           | `sysctl vfs.usermount`          |
| Membership of `operator`    | `id`                            |
| A cable that carries data   | See "A bad cable" below         |

Set the two system values one time:

```
doas sysctl vfs.usermount=1
doas sysrc -f /etc/sysctl.conf vfs.usermount=1
doas pw groupmod operator -m $USER
```

Log out and log in again, so the new group takes effect.

Your account needs the `operator` group for the `ugen` device. The mount then
runs as your account, and not as root. A mount that root makes needs the
`allow_other` option, and each file then belongs to root.

## Install

```
cargo build --release
doas install -m 755 target/release/mtpfs /usr/local/bin/mtpfs
doas cp tools/automount/bsdroid.conf /usr/local/etc/devd/
doas cp tools/automount/bsdroid-automount /usr/local/sbin/
doas cp tools/automount/bsdroid-automount.conf.sample \
    /usr/local/etc/bsdroid-automount.conf
```

Name your account in `/usr/local/etc/bsdroid-automount.conf`:

```
MOUNT_USER="jax"
```

The helper guesses the account for an empty name. Each guess can be wrong, so
write the name.

Give `devd` the vendor of your cellphone. Run `mtpfs -l` for the number:

```
$ mtpfs -l
NODE          ID            NAME
ugen0.11      04e8:6860    SAMSUNG SAMSUNG_Android
```

`04e8` is the vendor, and `6860` is the product. Put the vendor in both rules
in `/usr/local/etc/devd/bsdroid.conf`. Add one pair of rules for each vendor.

Start `devd` again:

```
doas service devd restart
```

## Test

Connect the cellphone. Unlock the cellphone. Select file transfer mode.

```
$ mount | grep mtpfs
mtpfs on /media/samsung-sm-s901u (fusefs, nosuid, mounted by jax)
```

Pull the cable. The folder goes away.

## What to watch for

### File transfer mode

The cellphone gives no files in charge mode. Select file transfer mode in the
notification shade of the cellphone.

Each USB mode is a different device on the bus. A change of mode therefore
removes the cellphone and adds the cellphone again.

A cable that comes back fast keeps the mode on some cellphones:

| Cellphone           | After a short pull of the cable |
| ------------------- | ------------------------------- |
| Samsung SM-S901U    | Keeps file transfer for about a second |
| Nuu N6501L          | Keeps file transfer              |
| Motorola Moto G (5) | Drops to charge mode at once     |

Select the mode again for a cellphone that drops to charge mode.

### A locked screen

The screen locks, and some cellphones then stop the session. Unlock the
cellphone and connect the cable again.

### A bad cable

A bad cable gives perfect reads and failed writes. The fault looks like a
fault of the software, and it is not.

The measurement of one bad cable and one good cable, with the same cellphone
and the same file:

| Cable        | Writes that passed |
| ------------ | ------------------ |
| The old one  | 0 of 10            |
| A new one    | 10 of 10           |

Check a cable that you doubt:

```
sh tools/check-write-errors.sh /media/samsung-sm-s901u ugen0.11
```

Try another cable before you read the code.

### Two mounts of one cellphone

A program that closes a session leaves the cellphone busy for about a second.
The next `OpenSession` then gives a timeout. `mtpfs` waits and tries again, so
an unmount and a mount in the same second work.

Two mounts of one cellphone at one time also work. The measurement on the
Samsung shows writes through both mount points, one after the other, with no
fault.

Do not use two mounts of one cellphone anyway. Each mount holds its own cache,
so a folder that you make through the first mount point is not there through
the second one. The two mount points disagree, and neither one is wrong.

### The space a change to a file needs

A change to a file that is already on the cellphone needs space in two places:

| Place                | How much             |
| -------------------- | -------------------- |
| The disk of the host | The size of the file |
| The cellphone        | The size of the file, one more time |

MTP has no operation that writes into the middle of a file. A change therefore
reads the whole file to the disk of the host, and sends the whole file back.

A change to one byte of a file of 4 GB reads 4 GB and writes 4 GB.

Set `BSDROID_SPOOL` for a folder with more room:

```
BSDROID_SPOOL=/var/tmp/big mtpfs ugen0.11 /mnt/phone
```

`/tmp` on some hosts lives in memory. Do not put the spool folder there.

The cellphone needs room for two copies, because the old copy stays until the
new copy is in place. A cellphone with too little room gets the other way: the
old copy goes first. The log says which way the host takes.

### Two cellphones of the same model

The name of the folder comes from the model, so two cellphones of one model
want one folder. The helper gives the second cellphone the next free name:

```
/media/samsung-sm-s901u
/media/samsung-sm-s901u-2
```

The number goes to the cellphone that arrives second, so the number is not a
name for one cellphone. Two cellphones of one model can therefore trade
folders between one plug and the next. Use the model name alone for a
bookmark, and give the second cellphone a mount by hand for a folder that must
not move:

```
mtpfs ugen0.12 /media/phone2
```

### The speed is the bus, and not MTP

A read reached 42.7 MiB each second on a USB 2.0 high speed link. 42.7 is
about three quarters of the limit of the link, which is the normal figure for a
bulk transfer.

MTP is not the limit. A faster link needs a cellphone, a port and a cable that
all give SuperSpeed.

### What MTP does not hold

| Missing   | What you see                          |
| --------- | ------------------------------------- |
| A time    | `ls -l` shows `Dec 31 1969`           |
| A mode    | Each file is `rw-r--r--`              |
| An owner  | Each file belongs to your account     |
| A link    | `ln -s` gives a fault                 |

`cp -p` works. The filesystem accepts the request for a time and a mode, and
changes nothing, because a fault there stops the copy.

## Read the log

The helper writes to syslog:

```
tail -f /var/log/messages | grep bsdroid
```

A normal cycle:

```
bsdroid-automount: ugen0.11 gives no MTP, so nothing is mounted
bsdroid-automount: mount samsung SM-S901U on /media/samsung-sm-s901u for jax
bsdroid-automount: /media/samsung-sm-s901u is ready
bsdroid-automount: unmount /media/samsung-sm-s901u
```

The first line is normal. The cellphone joins the bus in charge mode, and the
helper waits for the mode that gives MTP.

| Message                        | What to do                        |
| ------------------------------ | --------------------------------- |
| `gives no MTP`                 | Select file transfer mode         |
| `no account to mount for`      | Set `MOUNT_USER` in the settings  |
| `is not there`                 | Install `mtpfs` in `/usr/local/bin` |
| `cannot make /media/...`       | Check the room on the root disk   |
| `already holds a mount`        | Normal for a second event         |

Run the mount by hand for a fault that the log does not explain:

```
mtpfs -f ugen0.11 /mnt/phone
```

The option `-f` keeps the program in the foreground, and each message goes to
your terminal. Add `BSDROID_DEBUG=1` for more.

## Remove it

```
doas rm /usr/local/etc/devd/bsdroid.conf
doas rm /usr/local/sbin/bsdroid-automount
doas rm /usr/local/etc/bsdroid-automount.conf
doas service devd restart
```

The mount by hand still works after you remove the files above.

## Why the project works this way

A file manager talks to a filesystem. FUSE gives a filesystem, so Nautilus,
Dolphin and Thunar all get the same answer with no work of their own.

`devd` gives the event, and `devd` is in the base system. `autofs` does not
fit, because `autofs` mounts on a read of the path and not on the arrival of a
device. `docs/11-automount.md` holds the full reason.

The project talks to `libusb20` of FreeBSD, and not to the compatibility layer
above. The layer above cannot set a deadline, and a transfer with no deadline
is the reason for the reputation of MTP. `docs/00-why.md` holds the
measurement.
