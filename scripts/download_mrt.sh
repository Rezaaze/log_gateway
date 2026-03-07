#!/usr/bin/env bash
# download_mrt.sh — RIPE RIS MRT Archive Downloader
# Verwendung: ./scripts/download_mrt.sh [--days N] [--collectors "rrc12 rrc00"]
# Standard:   30 Tage, Kollektoren: rrc12 rrc00 rrc11 rrc17

set -euo pipefail

DAYS=30
COLLECTORS="rrc12 rrc00 rrc11 rrc17"
OUTPUT_DIR="data/mrt"
BASE_URL="https://data.ris.ripe.net"

# Parameter parsen
while [[ $# -gt 0 ]]; do
    case $1 in
        --days)       DAYS="$2";       shift 2 ;;
        --collectors) COLLECTORS="$2"; shift 2 ;;
        --output)     OUTPUT_DIR="$2"; shift 2 ;;
        *) echo "Unbekannter Parameter: $1"; exit 1 ;;
    esac
done

echo "Lade MRT-Daten: letzte ${DAYS} Tage, Kollektoren: ${COLLECTORS}"
echo "Zielverzeichnis: ${OUTPUT_DIR}"

total=0
skipped=0
downloaded=0

for collector in $COLLECTORS; do
    for day_offset in $(seq 0 $((DAYS - 1))); do
        # Datum berechnen (macOS und Linux kompatibel)
        if date --version &>/dev/null 2>&1; then
            # GNU date (Linux)
            date_str=$(date -d "${day_offset} days ago" +%Y.%m)
            date_file=$(date -d "${day_offset} days ago" +%Y%m%d)
        else
            # BSD date (macOS)
            date_str=$(date -v-${day_offset}d +%Y.%m)
            date_file=$(date -v-${day_offset}d +%Y%m%d)
        fi

        target_dir="${OUTPUT_DIR}/${collector}/${date_str}"
        mkdir -p "$target_dir"

        # Alle 5-Minuten-Files des Tages
        for hour in $(seq -w 0 23); do
            for minute in 00 05 10 15 20 25 30 35 40 45 50 55; do
                filename="updates.${date_file}.${hour}${minute}.gz"
                url="${BASE_URL}/${collector}/${date_str}/${filename}"
                target="${target_dir}/${filename}"

                total=$((total + 1))

                if [[ -f "$target" ]]; then
                    skipped=$((skipped + 1))
                    continue
                fi

                if curl -sf --max-time 30 -o "$target" "$url" 2>/dev/null; then
                    downloaded=$((downloaded + 1))
                    echo "✓ ${collector}/${date_str}/${filename}"
                else
                    rm -f "$target"  # leere Datei entfernen bei Fehler
                fi
            done
        done
    done
done

echo ""
echo "Fertig: ${downloaded} heruntergeladen, ${skipped} übersprungen (${total} gesamt)"