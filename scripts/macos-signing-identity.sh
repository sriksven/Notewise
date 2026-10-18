#!/usr/bin/env bash
# Create the certificate every macOS release of Notewise is signed with. Run it once, ever.
#
# # Why a certificate at all, without an Apple Developer ID
#
# macOS attaches the Microphone, Screen & System Audio Recording, Accessibility and Input
# Monitoring grants to an app's *designated requirement*. Signed ad-hoc, that requirement is a
# hash of the binary, so every update is a stranger and every grant is lost. Signed with a
# certificate — any certificate, self-signed included — it becomes
#
#     identifier "dev.notewise.app" and certificate leaf = H"<sha1 of this certificate>"
#
# which stays true from one release to the next. That is the whole reason for this script.
#
# # Why a script rather than Keychain Access
#
# Keychain Access's Certificate Assistant makes a certificate valid for one year unless you
# override its defaults, and `codesign` refuses an expired identity outright ("no identity
# found"). The replacement would have a different hash, so the release signed with it would reset
# every user's permissions. This makes one that outlives the problem, with the extensions code
# signing needs and nothing else.
#
# # What you must do with the output
#
# Keep the .p12 and its password in a password manager. Losing them is the same as letting the
# certificate expire. Nothing else in this repository can recreate them.
set -euo pipefail

name="${NOTEWISE_SIGNING_NAME:-Notewise Release Signing}"
days="${NOTEWISE_SIGNING_DAYS:-7305}" # twenty years
out="${1:-$HOME/notewise-signing}"

if [ -e "$out" ]; then
  echo "refusing to write into $out: it already exists, and this must never overwrite an identity" >&2
  echo "that releases have been signed with. Pass a different directory." >&2
  exit 1
fi

# `openssl` on macOS may be LibreSSL or OpenSSL 3; both understand everything below. The p12 is
# written with the older PBE algorithms because `security import`, which the release workflow
# uses, does not read every newer OpenSSL default.
command -v openssl > /dev/null || { echo "needs openssl" >&2; exit 1; }

if [ -z "${NOTEWISE_SIGNING_PASSWORD:-}" ]; then
  read -rsp "Password for the .p12 (at least 12 characters): " NOTEWISE_SIGNING_PASSWORD; echo
  read -rsp "Again: " confirm; echo
  [ "$NOTEWISE_SIGNING_PASSWORD" = "$confirm" ] || { echo "the passwords differ" >&2; exit 1; }
fi
[ "${#NOTEWISE_SIGNING_PASSWORD}" -ge 12 ] || { echo "use at least 12 characters" >&2; exit 1; }

umask 077
mkdir -p "$out"
work=$(mktemp -d)
trap 'rm -f "$work"/key.pem "$work"/cert.pem; rmdir "$work"' EXIT

openssl req -x509 -newkey rsa:4096 -sha256 -nodes \
  -keyout "$work/key.pem" -out "$work/cert.pem" -days "$days" \
  -subj "/CN=$name" \
  -addext "basicConstraints=critical,CA:false" \
  -addext "keyUsage=critical,digitalSignature" \
  -addext "extendedKeyUsage=critical,codeSigning" 2> /dev/null

PASSWORD="$NOTEWISE_SIGNING_PASSWORD" openssl pkcs12 -export \
  -inkey "$work/key.pem" -in "$work/cert.pem" -name "$name" \
  -keypbe PBE-SHA1-3DES -certpbe PBE-SHA1-3DES -macalg sha1 \
  -passout env:PASSWORD -out "$out/notewise-signing.p12"

cp "$work/cert.pem" "$out/notewise-signing.pem"

sha1=$(openssl x509 -in "$work/cert.pem" -noout -fingerprint -sha1 | cut -d= -f2 | tr -d :)
expires=$(openssl x509 -in "$work/cert.pem" -noout -enddate | cut -d= -f2)

cat << NEXT

Created in $out:
  notewise-signing.p12   private key + certificate. The secret. Back it up with its password.
  notewise-signing.pem   the certificate alone. Public; safe to share.

  Name:         $name
  SHA-1:        $sha1
  Expires:      $expires

Next:

1. Put the .p12 and its password in your password manager now.

2. Add two repository secrets (Settings → Secrets and variables → Actions):
     MACOS_CERTIFICATE           base64 -i "$out/notewise-signing.p12" | pbcopy
     MACOS_CERTIFICATE_PASSWORD  the password you just typed

3. Pin the certificate, so a release can never be signed with a different one by accident.
   In .github/workflows/release.yml set:
     MACOS_CERTIFICATE_SHA1: "$sha1"

4. Optional, for signed builds on this Mac:
     security import "$out/notewise-signing.p12" -k ~/Library/Keychains/login.keychain-db -T /usr/bin/codesign
     APPLE_SIGNING_IDENTITY=$sha1 npx tauri build --features bundle --bundles dmg
NEXT
