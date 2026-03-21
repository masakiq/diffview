#!/bin/zsh

set -euo pipefail

script_dir=${0:A:h}
repo_root=${script_dir:h:h}
cd "${repo_root}"

bin_path="./target/release/diffview"
tool="raw"
commit_rev=""
runs=20
warmup=3
moves=200
settle_ms=300
output_root="${repo_root}/perf/results"
skip_build=0
dry_run=0

usage() {
  cat <<'EOF'
Usage: perf/scripts/run_measurements.sh [options]

Options:
  --bin PATH           diffview binary path (default: ./target/release/diffview)
  --tool TOOL          diff tool to benchmark (default: raw)
  --commit-rev REV     revision for commit-mode benchmarks
  --runs N             hyperfine runs (default: 20)
  --warmup N           hyperfine warmup runs (default: 3)
  --moves N            move count for burst benchmark (default: 200)
  --settle-ms N        wait after key input for preview update (default: 300)
  --output-dir PATH    root directory for benchmark results (default: perf/results)
  --skip-build         skip cargo build --release
  --dry-run            print commands without executing hyperfine
  --help               show this help
EOF
}

require_command() {
  local cmd=$1
  if ! command -v "$cmd" >/dev/null 2>&1; then
    print -u2 "Missing required command: $cmd"
    exit 127
  fi
}

resolve_path() {
  local path=$1
  if [[ "$path" = /* ]]; then
    print -- "$path"
  else
    print -- "${repo_root}/${path}"
  fi
}

run_cmd() {
  print -- "+ $*"
  if (( ! dry_run )); then
    "$@"
  fi
}

quote_cmd() {
  local -a parts
  parts=("$@")
  printf "%q " "${parts[@]}"
}

run_hyperfine() {
  local name=$1
  shift
  local -a command=("$@")
  local stdout_file="${run_dir}/${name}.txt"
  local json_file="${run_dir}/${name}.json"
  local quoted_command

  quoted_command=$(quote_cmd "${command[@]}")

  print -- "== ${name} =="
  print -- "Command: ${quoted_command}"

  if (( dry_run )); then
    print -- "Output: ${stdout_file}"
    print -- "JSON:   ${json_file}"
    return
  fi

  hyperfine \
    --warmup "${warmup}" \
    --runs "${runs}" \
    --export-json "${json_file}" \
    "${quoted_command}" | tee "${stdout_file}"
}

while (( $# > 0 )); do
  case "$1" in
    --bin)
      bin_path=$2
      shift 2
      ;;
    --tool)
      tool=$2
      shift 2
      ;;
    --commit-rev)
      commit_rev=$2
      shift 2
      ;;
    --runs)
      runs=$2
      shift 2
      ;;
    --warmup)
      warmup=$2
      shift 2
      ;;
    --moves)
      moves=$2
      shift 2
      ;;
    --settle-ms)
      settle_ms=$2
      shift 2
      ;;
    --output-dir)
      output_root=$2
      shift 2
      ;;
    --skip-build)
      skip_build=1
      shift
      ;;
    --dry-run)
      dry_run=1
      shift
      ;;
    --help)
      usage
      exit 0
      ;;
    *)
      print -u2 "Unknown option: $1"
      usage
      exit 2
      ;;
  esac
done

startup_script="${script_dir}/startup.expect"
tree_burst_script="${script_dir}/tree_burst.expect"
tree_single_script="${script_dir}/tree_single_step.expect"

if (( ! dry_run )); then
  require_command cargo
  require_command hyperfine
  require_command expect
fi

if [[ ! -x "${startup_script}" || ! -x "${tree_burst_script}" || ! -x "${tree_single_script}" ]]; then
  print -u2 "Required perf scripts are missing under ${script_dir}"
  exit 1
fi

if (( ! skip_build )); then
  run_cmd cargo build --release
fi

bin_abs=$(resolve_path "${bin_path}")
if (( ! dry_run )) && [[ ! -x "${bin_abs}" ]]; then
  print -u2 "Benchmark binary is not executable: ${bin_abs}"
  exit 1
fi

if [[ -n "${commit_rev}" ]]; then
  if (( dry_run )); then
    :
  elif ! git -C "${repo_root}" rev-parse --verify "${commit_rev}^{commit}" >/dev/null 2>&1; then
    print -u2 "Invalid commit revision: ${commit_rev}"
    exit 1
  fi
fi

timestamp=$(date +"%Y%m%d-%H%M%S")
run_dir="$(resolve_path "${output_root}")/${timestamp}"
if (( ! dry_run )); then
  run_cmd mkdir -p "${run_dir}"
fi

if (( dry_run )); then
  print -- "Dry run output directory: ${run_dir}"
else
  cat > "${run_dir}/metadata.txt" <<EOF
tool=${tool}
runs=${runs}
warmup=${warmup}
moves=${moves}
settle_ms=${settle_ms}
bin=${bin_abs}
commit_rev=${commit_rev}
repo_root=${repo_root}
EOF
fi

worktree_cmd=("${startup_script}" "${bin_abs}" "--tool" "${tool}")
worktree_burst_cmd=("${tree_burst_script}" "--moves" "${moves}" "--settle-ms" "${settle_ms}" "--" "${bin_abs}" "--tool" "${tool}")
worktree_single_cmd=("${tree_single_script}" "--settle-ms" "${settle_ms}" "--" "${bin_abs}" "--tool" "${tool}")

run_hyperfine "startup_worktree" "${worktree_cmd[@]}"
run_hyperfine "tree_burst_worktree" "${worktree_burst_cmd[@]}"
run_hyperfine "tree_single_step_worktree" "${worktree_single_cmd[@]}"

if [[ -n "${commit_rev}" ]]; then
  commit_start_cmd=("${startup_script}" "${bin_abs}" "--tool" "${tool}" "${commit_rev}")
  commit_burst_cmd=("${tree_burst_script}" "--moves" "${moves}" "--settle-ms" "${settle_ms}" "--" "${bin_abs}" "--tool" "${tool}" "${commit_rev}")
  commit_single_cmd=("${tree_single_script}" "--settle-ms" "${settle_ms}" "--" "${bin_abs}" "--tool" "${tool}" "${commit_rev}")

  run_hyperfine "startup_commit" "${commit_start_cmd[@]}"
  run_hyperfine "tree_burst_commit" "${commit_burst_cmd[@]}"
  run_hyperfine "tree_single_step_commit" "${commit_single_cmd[@]}"
fi

print -- "Results written to ${run_dir}"
