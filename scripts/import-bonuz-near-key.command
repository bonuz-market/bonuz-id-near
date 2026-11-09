#!/bin/bash
# Saves the bonuz.near key on this Mac so Claude can deploy the bonuz ID registry
# on NEAR mainnet. The key is checked on NEAR first (must be a full-access key of
# bonuz.near), saved owner-only in ~/.near-credentials/mainnet/, never shown.
cd "$HOME/repos/bonuz-id-near" || { echo "Folder ~/repos/bonuz-id-near is missing. Tell Claude."; read -r -p "Press Enter to close." _; exit 1; }

echo "bonuz.near key for NEAR mainnet"
echo
echo "Paste the private key (starts with ed25519:) or the 12/24-word seed phrase,"
echo "then press Enter. Nothing shows while you paste; that is normal."
read -rs SECRET
echo
if [ -z "$SECRET" ]; then
  echo "Nothing was pasted. Double-click the file again."
  read -r -p "Press Enter to close." _; exit 1
fi

printf '%s' "$SECRET" | node scripts/import-account-key.ts bonuz.near mainnet
STATUS=$?
unset SECRET
echo
if [ $STATUS -eq 0 ]; then
  echo "Saved. Go back to Claude and say done."
else
  echo "Not saved. Read the line above, then double-click again with the right key."
fi
read -r -p "Press Enter to close." _
