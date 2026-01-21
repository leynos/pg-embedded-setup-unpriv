#!/bin/sh
set -eu
data_dir=""
expect_data=0
for arg in "$@"; do
  if [ "$expect_data" -eq 1 ]; then
    data_dir="$arg"
    break
  fi
  case "$arg" in
    -D)
      expect_data=1
      ;;
    -D*)
      data_dir="${arg#-D}"
      break
      ;;
    --pgdata)
      expect_data=1
      ;;
    --pgdata=*)
      data_dir="${arg#--pgdata=}"
      break
      ;;
  esac
done
if [ -z "$data_dir" ]; then
  echo "missing -D argument" >&2
  exit 1
fi
mkdir -p "$data_dir"
echo "12345" > "$data_dir/postmaster.pid"
