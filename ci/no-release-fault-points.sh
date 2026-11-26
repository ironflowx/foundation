#!/usr/bin/env bash
# Usage: ci/no-release-fault-points.sh [release.yml] [Cargo.toml]
# Archive builds use an explicit, package-only Cargo command in release.yml. A workspace build can
# unify in the hooks from the unpublished crash-test crate. Commands hidden in another script or
# constructed from expressions are outside this check; keep archive build arguments in release.yml.
set -euo pipefail

workflow=${1:-.github/workflows/release.yml}
# shellcheck disable=SC2016 # Ruby interpolates, the shell should not.
ruby -ryaml -rshellwords -e '
  path = ARGV.fetch(0)
  workflow = YAML.load_file(path)
  def strings(node)
    case node
    when Hash then node.values.flat_map { |value| strings(value) }
    when Array then node.flat_map { |value| strings(value) }
    when String then [node]
    else []
    end
  end
  inputs = strings(workflow).flat_map(&:lines).reject { |line| line.lstrip.start_with?("#") }
  bad = []
  bad << "release workflow names __fault-points" if inputs.any? { |line| line.include?("__fault-points") }
  bad << "release workflow enables --all-features" if inputs.any? { |line| line.include?("--all-features") }
  workflow.fetch("jobs").each_value do |job|
    job.fetch("steps", []).each do |step|
      next unless step["run"]
      joined = step["run"].gsub(/\\\n/, " ")
      joined.each_line do |line|
        next if line.lstrip.start_with?("#")
        line.split(/[;&|]/).each do |command|
          build = /(?:^|[[:space:]])cargo(?:\s+\+\S+)?\s+(?:build|b|rustc)(?:\s|$)/.match(command)
          next unless build
          args = Shellwords.split(build.post_match)
          packages = args.each_index.map do |i|
            case args[i]
            when "-p", "--package" then args[i + 1]
            when /\A--package=(.+)\z/, /\A-p(.+)\z/ then Regexp.last_match(1)
            end
          end.compact
          broad = args.any? { |arg| arg.match?(/\A--(?:workspace|all|all-targets|tests?|examples?|benches|bench)(?:=|\z)/) }
          if packages != ["ironflowx-bin"] || broad
            bad << "release Cargo build must select only ironflowx-bin without dev targets"
          end
        end
      end
    end
  end
  puts bad.uniq.map { |message| "#{path}: #{message}" }
  exit bad.empty?
' "$workflow"

manifest=${2:-Cargo.toml}
# Even a regular opt-in feature of the host must not be an alias for a crash feature.
ordinary=$(cargo metadata --locked --manifest-path "$manifest" --no-deps --format-version 1 |
  jq -er 'first(.packages[] | select(.name == "ironflowx-bin"))
    | .features | keys | map(select(. != "__fault-points")) | join(",")')
features=$(cargo tree --locked --manifest-path "$manifest" -p ironflowx-bin \
  --target all --edges normal,build --features "$ordinary" \
  --prefix none --format '{p} [{f}]')
if grep -qE '[[,]__fault-points[],]' <<<"$features"; then
  echo "ironflowx-bin: ordinary release graph enables __fault-points"
  exit 1
fi
