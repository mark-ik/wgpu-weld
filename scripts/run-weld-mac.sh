#!/usr/bin/env bash
# Launch a bundled demo-weld-mac through the logged-in LaunchServices session
# and wait for the exact application executable to finish. `open -W` is not a
# reliable wait primitive on self-hosted macOS runners: the app can launch
# successfully while `open` reports that it could not obtain a PID.

set -u

if [[ $# -lt 4 ]]; then
  echo "usage: $0 APP STDOUT STDERR TIMEOUT_SECS [KEY=VALUE ...]" >&2
  exit 2
fi

app="$1"
stdout="$2"
stderr="$3"
timeout_secs="$4"
shift 4

case "$app" in
*.app) ;;
*)
  echo "app path must end in .app: $app" >&2
  exit 2
  ;;
esac

executable="$app/Contents/MacOS/demo-weld-mac"
if [[ ! -x "$executable" ]]; then
  echo "demo-weld-mac executable not found at $executable" >&2
  exit 2
fi

mkdir -p "$(dirname "$stdout")" "$(dirname "$stderr")"
: >"$stdout"
if [[ "$stderr" != "$stdout" ]]; then
  : >"$stderr"
fi

app_running() {
  while read -r _pid command; do
    if [[ "$command" = *"$executable"* ]]; then
      return 0
    fi
  done < <(ps -ax -o pid= -o command=)
  return 1
}

stop_app() {
  while read -r pid command; do
    if [[ "$command" = *"$executable"* ]]; then
      kill "$pid" 2>/dev/null || true
    fi
  done < <(ps -ax -o pid= -o command=)
  return 0
}
trap stop_app EXIT

open_args=(-n -F --stdout "$stdout" --stderr "$stderr")
for assignment in "$@"; do
  open_args+=(--env "$assignment")
done

stop_app
if ! open "${open_args[@]}" "$app"; then
  exit 1
fi

deadline=$((SECONDS + timeout_secs))
launch_deadline=$((SECONDS + 5))
saw_process=0
while [[ $SECONDS -lt $deadline ]]; do
  if app_running; then
    saw_process=1
  elif [[ $saw_process -eq 1 ]]; then
    exit 0
  elif [[ $SECONDS -ge $launch_deadline && ( -s "$stdout" || -s "$stderr" ) ]]; then
    # A very short-lived process can fall between polls. Its redirected output
    # is still sufficient for the caller to judge the required receipt.
    exit 0
  fi
  sleep 0.1
done

exit 142
