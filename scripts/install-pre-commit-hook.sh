#!/bin/bash
#
# Script to install the pre-commit hook for secret scanning
#

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
HOOK_PATH="$REPO_ROOT/.git/hooks/pre-commit"

# Create the pre-commit hook
cat > "$HOOK_PATH" << 'EOF'
#!/bin/bash
#
# Pre-commit hook to scan for secrets using GitGuardian ggshield
# This hook prevents commits containing secrets from being made
#

set -e

# Colors for output
RED='\033[0;31m'
YELLOW='\033[1;33m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

# Check if ggshield is installed
if ! command -v ggshield &> /dev/null; then
    echo -e "${YELLOW}Warning: ggshield not found. Install it with:${NC}"
    echo "  pip install ggshield"
    echo ""
    echo -e "${YELLOW}Skipping secret scanning for this commit.${NC}"
    echo -e "${YELLOW}To install ggshield: pip install ggshield${NC}"
    exit 0
fi

# Check if GITGUARDIAN_API_KEY is set (optional but recommended)
if [ -z "$GITGUARDIAN_API_KEY" ]; then
    echo -e "${YELLOW}Notice: GITGUARDIAN_API_KEY not set. Using offline detection only.${NC}"
    echo "For enhanced detection, set your GitGuardian API key:"
    echo "export GITGUARDIAN_API_KEY=your_api_key_here"
    echo ""
fi

echo -e "${GREEN}Scanning staged files for secrets...${NC}"

# Get list of staged files (excluding deleted files)
STAGED_FILES=$(git diff --cached --name-only --diff-filter=ACM)

if [ -z "$STAGED_FILES" ]; then
    echo "No staged files to scan."
    exit 0
fi

# Run ggshield on staged files
echo "Files to scan:"
echo "$STAGED_FILES" | sed 's/^/  - /'
echo ""

# Use ggshield to scan the staged files
if echo "$STAGED_FILES" | xargs ggshield secret scan pre-commit; then
    echo -e "${GREEN}✓ No secrets detected in staged files${NC}"
    exit 0
else
    echo -e "${RED}✗ Secrets detected in staged files!${NC}"
    echo ""
    echo -e "${RED}Commit aborted to prevent secret leakage.${NC}"
    echo ""
    echo "To resolve this:"
    echo "1. Remove or encrypt the detected secrets"
    echo "2. Add them to .gitguardian.yml if they are false positives"
    echo "3. Use environment variables or external config files"
    echo "4. Ensure sensitive files are in .gitignore"
    echo ""
    echo "For emergencies only (NOT recommended):"
    echo "  git commit --no-verify  # bypasses this hook"
    exit 1
fi
EOF

# Make the hook executable
chmod +x "$HOOK_PATH"

echo "✓ Pre-commit hook installed successfully at $HOOK_PATH"
echo "  The hook will now scan for secrets before each commit."
echo ""
echo "To install ggshield (if not already installed):"
echo "  pip install ggshield"
echo ""
echo "For enhanced detection, set your GitGuardian API key:"
echo "  export GITGUARDIAN_API_KEY=your_api_key_here"