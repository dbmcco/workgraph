#!/bin/bash
# Script to replace leaked Telegram bot token
# Usage: ./replace_token.sh NEW_TOKEN_HERE

if [ $# -ne 1 ]; then
    echo "Usage: $0 NEW_TOKEN"
    echo "Example: $0 XXXXXXXXXX:PLACEHOLDER_TOKEN"
    exit 1
fi

NEW_TOKEN="$1"
CONFIG_FILE="$HOME/.config/workgraph/notify.toml"
BACKUP_FILE="$HOME/.config/workgraph/notify.toml.backup-$(date +%Y%m%d-%H%M%S)"

echo "🔧 Replacing Telegram bot token..."
echo "📋 Old token: ***REDACTED-TOKEN***"
echo "📋 New token: $NEW_TOKEN"

# Create backup
echo "💾 Creating backup: $BACKUP_FILE"
cp "$CONFIG_FILE" "$BACKUP_FILE"

# Replace token in config file
echo "✏️  Updating config file..."
sed -i "s/bot_token = \"***REDACTED-TOKEN***\"/bot_token = \"$NEW_TOKEN\"/" "$CONFIG_FILE"

echo "✅ Token replacement complete!"
echo "📁 Backup saved to: $BACKUP_FILE"
echo "📁 Config updated: $CONFIG_FILE"