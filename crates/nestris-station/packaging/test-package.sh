#!/bin/sh
# CI check of the Debian package in a clean container (no systemd there, so
# the update helper is run by hand):
#   docker run --rm -v "$PWD:/w" -w /w ubuntu:24.04 sh crates/nestris-station/packaging/test-package.sh target/debian/<deb>
set -eu
DEB="$1"
DATA=crates/nestris-station/tests/data
UPD=/var/lib/nestris-station/updates

fail() { echo "FAIL: $*"; exit 1; }
result_has() { grep -q "$1" "$UPD/result" || { cat "$UPD/result"; fail "result lacks $1"; }; }

export DEBIAN_FRONTEND=noninteractive
apt-get update -qq
apt-get install -y -qq "./$DEB" > /tmp/apt.log 2>&1 || { tail -n 30 /tmp/apt.log; fail "install"; }

echo "== files and units"
test -x /usr/lib/nestris-station/update-helper || fail "helper not executable"
for unit in nestris-station.service nestris-station-update.path nestris-station-update.service; do
    ls /lib/systemd/system/$unit /usr/lib/systemd/system/$unit 2>/dev/null | grep -q . || fail "$unit missing"
done
ls /etc/systemd/system/multi-user.target.wants/ | grep -q nestris-station-update.path \
    || fail "path unit not enabled"
getent passwd nestris > /dev/null || fail "user nestris missing"

echo "== verify-update with a real signed release"
mkdir -p /tmp/rel
cp "$DATA/reader-v1.0.0-SHA256SUMS.txt" /tmp/rel/SHA256SUMS.txt
cp "$DATA/reader-v1.0.0-SHA256SUMS.txt.sig" /tmp/rel/SHA256SUMS.txt.sig
cp "$DATA/nestris-rfid-reader-1.0.0-manifest.json" /tmp/rel/
nestris-station verify-update /tmp/rel nestris-rfid-reader-1.0.0-manifest.json || fail "real file rejected"
echo tampered >> /tmp/rel/nestris-rfid-reader-1.0.0-manifest.json
if nestris-station verify-update /tmp/rel nestris-rfid-reader-1.0.0-manifest.json; then fail "tampered file accepted"; fi

echo "== helper: bad request"
install -d -o nestris -g nestris -m 0750 "$UPD"
echo "deb=../../etc/passwd" > "$UPD/request"
/usr/lib/nestris-station/update-helper && fail "bad request accepted"
test ! -e "$UPD/request" || fail "request not removed"
result_has '"ok":false'
result_has 'bad request'

echo "== helper: package without a valid release signature"
DEBNAME=nestris-station_9.9.9_amd64.deb
cp "./$DEB" "$UPD/$DEBNAME"
sha256sum "$UPD/$DEBNAME" | sed "s#$UPD/##" > "$UPD/SHA256SUMS.txt"
# A well-formed release signature, but of other data (the reader v1.0.0 list).
echo "c+1ROYdf80Dm0s+nxqUxrqSD/wP2BHGfC8H9ZOnFZo6RNQ++7/ehIlV4mLxkcoFwHRDJP6QAEUiCT3EX37NfAQ==" > "$UPD/SHA256SUMS.txt.sig"
echo "deb=$DEBNAME" > "$UPD/request"
/usr/lib/nestris-station/update-helper && fail "unsigned package accepted"
result_has 'verification failed'
test ! -e "$UPD/$DEBNAME" || fail "package left in the station's directory"
test "$(stat -c %U "$UPD/result")" = nestris || fail "result not owned by nestris"

echo "OK: package and update helper"
