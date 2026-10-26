COMMIT_NAME=$1

if [ -z "$COMMIT_NAME"]; then
  echo "❌ no commit name found"
  exit 1
fi

git add .
git commit -m "$COMMIT_NAME"
git push rustle main

echo "Commit finished successfully under $COMMIT_NAME"