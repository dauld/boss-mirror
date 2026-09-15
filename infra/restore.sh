#!/usr/bin/env bash
# Restore Boss from a backup tarball.
#
# Restores: Postgres database, gateway config, service configs, systemd units.
#
# Usage: sudo ./infra/restore.sh /var/backups/boss/boss-backup-YYYYMMDD-HHMMSS.tar.gz

set -euo pipefail

if [ $# -ne 1 ]; then
  echo "Usage: $0 <backup-tarball>"
  echo "Available backups:"
  ls -lh /var/backups/boss/boss-backup-*.tar.gz 2>/dev/null || echo "  (none found)"
  exit 1
fi

TARBALL="$1"
if [ ! -f "$TARBALL" ]; then
  echo "Error: $TARBALL not found"
  exit 1
fi

WORKDIR=$(mktemp -d)
echo "Boss restore from: $TARBALL"

# Extract
tar -xzf "$TARBALL" -C "$WORKDIR"
BACKUP=$(ls -d "$WORKDIR"/boss-backup-* | sed -n 1p)

if [ ! -d "$BACKUP" ]; then
  echo "Error: no backup directory found in tarball"
  rm -rf "$WORKDIR"
  exit 1
fi

# Stop services
echo "  stopping services..."
systemctl stop boss-assets-api boss-catalog-api boss-commerce-api boss-people-api boss-shipping-api boss-messages-api boss-inventory-api boss-gateway 2>/dev/null || true

# Restore Postgres
if [ -f "$BACKUP/boss.sql" ]; then
  echo "  restoring Postgres..."
  sudo -u postgres psql -d boss -c "DROP SCHEMA public CASCADE; CREATE SCHEMA public; GRANT ALL ON SCHEMA public TO boss; GRANT ALL ON SCHEMA public TO public;" 2>/dev/null
  sudo -u postgres psql -d boss -f "$BACKUP/boss.sql" > /dev/null 2>&1
  echo "  Postgres restored."
else
  echo "  (skipped — no boss.sql in backup)"
fi

# Restore gateway config
if [ -d "$BACKUP/boss-gateway" ]; then
  echo "  restoring gateway config..."
  cp -a "$BACKUP/boss-gateway/"* /etc/boss-gateway/ 2>/dev/null || true
fi

# Restore service configs
echo "  restoring service configs..."
for f in "$BACKUP"/boss-*.toml; do
  [ -f "$f" ] && cp "$f" /etc/
done

# Restore systemd units
if [ -d "$BACKUP/systemd" ]; then
  echo "  restoring systemd units..."
  cp "$BACKUP/systemd/"*.service /etc/systemd/system/ 2>/dev/null || true
  systemctl daemon-reload
fi

# Restart services
echo "  starting services..."
systemctl start boss-gateway boss-assets-api boss-catalog-api boss-commerce-api boss-people-api boss-shipping-api boss-messages-api boss-inventory-api 2>/dev/null || true

rm -rf "$WORKDIR"
echo "done. Verify with: sudo -u postgres psql -d boss -c 'SELECT count(*) FROM asset_models;'"
