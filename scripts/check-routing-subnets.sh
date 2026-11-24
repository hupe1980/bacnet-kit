#!/bin/sh
# Run with a Python interpreter containing BACpypes3 0.1.0.
# Every interface, route and sysctl below belongs to a fresh network namespace.
set -eu
python=${1:?supply the Python interpreter with BACpypes3 installed}
script_dir=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd)
exec unshare --mount --net --propagation private sh -seu -- "$python" "$script_dir" <<'TOPOLOGY'
    mkdir -p /run/netns
    mount -t tmpfs tmpfs /run/netns
    ip netns add gateway
    ip netns add workstation
    trap "ip netns del workstation; ip netns del gateway" EXIT

    ip link set lo up
    ip link add plant type veth peer name west netns gateway
    ip link add east netns gateway type veth peer name service netns workstation
    ip addr add 192.0.2.1/24 dev plant
    ip link set plant up
    ip -n gateway addr add 192.0.2.254/24 dev west
    ip -n gateway addr add 198.51.100.254/24 dev east
    ip -n gateway link set west up
    ip -n gateway link set east up
    ip netns exec gateway sysctl -qw net.ipv4.ip_forward=1
    ip -n workstation link set lo up
    ip -n workstation addr add 198.51.100.2/24 dev service
    ip -n workstation link set service up
    ip -n workstation route add default via 198.51.100.254
    ip route add 198.51.100.0/24 via 192.0.2.254
    "$1" "$2/check-routing.py" --uplink 192.0.2.1/24 --client-netns workstation
TOPOLOGY
