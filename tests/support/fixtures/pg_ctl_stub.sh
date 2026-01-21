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
    initdb|start|stop|status)
      command="$arg"
      ;;
  esac
done

if [ -z "$data_dir" ]; then
  echo "missing -D argument" >&2
  exit 1
fi

case "$command" in
  initdb)
    mkdir -p "$data_dir"
    echo "16" > "$data_dir/PG_VERSION"
    echo "pg_ctl_stub initdb: created PG_VERSION in $data_dir" >&2
    ;;
  start)
    mkdir -p "$data_dir"
    echo "12345" > "$data_dir/postmaster.pid"
    echo "pg_ctl_stub start: created postmaster.pid in $data_dir" >&2
    ;;
  stop)
    rm -f "$data_dir/postmaster.pid"
    echo "pg_ctl_stub stop: removed postmaster.pid from $data_dir" >&2
    ;;
  status)
    if [ -f "$data_dir/postmaster.pid" ]; then
      echo "pg_ctl_stub status: running (found postmaster.pid in $data_dir)" >&2
      exit 0
    else
      echo "pg_ctl_stub status: not running (no postmaster.pid in $data_dir)" >&2
      exit 3
    fi
    ;;
  *)
    echo "unknown command: $command" >&2
    exit 1
    ;;
esac
