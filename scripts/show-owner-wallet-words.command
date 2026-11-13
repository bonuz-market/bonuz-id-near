#!/bin/bash
# Shows the 12 words of the bonuz-id.near owner wallet ONCE, in this Terminal window only,
# so they can be written on paper. Nothing is copied, sent or saved anywhere else.
SEED="${OWNER_WALLET_SEED_FILE:-$HOME/.near-credentials/mainnet/owner-wallet.seed.txt}"
clear
echo "bonuz-id.near owner wallet: backup words"
echo
if [ ! -f "$SEED" ]; then
  echo "The wallet file is not on this Mac. Tell Claude."
  read -r -p "Press Enter to close. " _
  exit 1
fi
echo "Anyone who sees these 12 words controls bonuz-id.near and its NEAR."
echo "Make sure nobody is looking and nothing is recording your screen."
echo
read -r -p "Type SHOW and press Enter to see them: " ANSWER
if [ "$ANSWER" != "SHOW" ]; then
  echo "Nothing shown."
  exit 0
fi
echo
n=1
for w in $(cat "$SEED"); do
  printf '  %2d. %s\n' "$n" "$w"
  n=$((n + 1))
done
echo
echo "Write them on paper, in this order. Keep the paper somewhere safe, offline."
echo "Never type them into a website, a chat or a photo."
echo
read -r -p "Press Enter when done. The words disappear from this window. " _
clear
printf '\033[3J'
echo "Done. Close this window."
