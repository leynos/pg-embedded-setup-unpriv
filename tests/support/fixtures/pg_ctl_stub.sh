#!/bin/sh
set -eu

command=""
data_dir=""
expect_data=0
for arg in "$@"; do
  if [ "$expect_data" -eq 1 ]; then
    data_dir="$arg"
    expect_data=0
    continue
  fi
  case "$arg" in
    -D)
      expect_data=1
      ;;
    -D*)
      data_dir="${arg#-D}"
      ;;
    --pgdata)
      expect_data=1
      ;;
    --pgdata=*)
      data_dir="${arg#--pgdata=}"
      ;;
    start|stop|status)
      command="$arg"
      ;;
  esac
done

if [ -z "$data_dir" ]; then
  echo "missing -D argument" >&2
  exit 1
fi

case "$command" in
  start)
    mkdir -p "$data_dir"
    echo "12345" > "$data_dir/postmaster.pid"
    ;;
  stop)
    rm -f "$data_dir/postmaster.pid"
    ;;
  status)
    if [ -f "$data_dir/postmaster.pid" ]; then
      exit 0
    else
      exit 3
    fi
    ;;
  *)
    echo "unknown command: $command" >&2
    exit 1
    ;;
esac
