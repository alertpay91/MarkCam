# linux-v4l2

Linux virtual camera via the **v4l2loopback** kernel module — no separate user-space
consumer needed. The engine writes frames straight to the loopback node.

```sh
sudo modprobe v4l2loopback devices=1 video_nr=10 card_label="OpenVCam" exclusive_caps=1
```

`ovc-vcam`'s Linux backend opens `/dev/video10`, sets the format with `VIDIOC_S_FMT`, and
`write()`s frames. This folder holds the setup helper + a systemd/udev snippet to create the
device on login.
