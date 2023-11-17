# VSOCK file transfer over gRPC PoC for Nanos

This is a simple proof-of-concept project to represent gRPC service that
provides streaming file transfer service.

## Usage:

1 Build project (assuming [Rust installed](https://www.rust-lang.org/learn/get-started)):
```
  cargo build --release
```

2 Install [Ops](https://ops.city/) (if not done yet):
```
  curl https://ops.city/get.sh -sSfL | sh
```

3 Create workspace volume:
```
  ops volume create -t onprem -s 2g workspace
```

4. Ensure that Linux VSOCK driver is loaded:
```
  sudo modprobe vhost_vsock
```

5 Construct QEMU command (and build Nanos image)
```
  ops run ./target/release/vsock-client -c local.json --show-debug -v
```

The output contains the QEMU command for execution. Copy it, and add `-device vhost-vsock-pci,guest-cid=5` to the end. That enables VSOCK.

6. Run the server:
```
  ./target/release/vsock-server
```

7. Run the Nanos VM under QEMU:

Here, take the QEMU command constructed earlier, in step 5., and execute it:
```
  qemu-system-x86_64 -machine q35 -device pcie-root-port,port=0x10,chassis=1,id=pci.1,bus=pcie.0,multifunction=on,addr=0x3 -device pcie-root-port,port=0x11,chassis=2,id=pci.2,bus=pcie.0,addr=0x3.0x1 -device pcie-root-port,port=0x12,chassis=3,id=pci.3,bus=pcie.0,addr=0x3.0x2 -device virtio-scsi-pci,bus=pci.2,addr=0x0,id=scsi0 -device scsi-hd,bus=scsi0.0,drive=hd0 -vga none -smp 1 -device isa-debug-exit -m 8g -device virtio-rng-pci -device scsi-hd,bus=scsi0.0,drive=hd1 -machine accel=kvm:tcg -cpu host -no-reboot -cpu max -drive file=/home/user/.ops/images/vsock-client,format=raw,if=none,id=hd0 -drive file=/home/user/.ops/volumes/workspace:c0aa64ba-005a-b591-39e2-e3168555089f.raw,format=raw,if=none,id=hd1 -device virtio-net,bus=pci.3,addr=0x0,netdev=n0,mac=3a:e5:12:9d:86:27 -netdev user,id=n0 -display none -serial stdio -device vhost-vsock-pci,guest-cid=5
```
