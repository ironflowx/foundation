#!/usr/bin/env bash
# Usage: ci/no-release-fault-points.sh [release.yml] [Cargo.toml]
# Archive builds use an explicit, package-only Cargo command in release.yml. A workspace build can
# unify in the hooks from the unpublished crash-test crate. Commands hidden in another script or
# constructed from expressions are outside this check; keep archive build arguments in release.yml.
set -euo pipefail

workflow=${1:-.github/workflows/release.yml}
ruby -ryaml -rshellwords - "$workflow" <<'RUBY'
  # Tokenize literal commands without evaluating shell expansions. Shellwords decodes quoting.
  def commands(script)
    script.gsub(/\\\n/, " ").lines.flat_map do |line|
      sections = [[]]
      line.scan(/(?:\\.|"[^"\\]*(?:\\.[^"\\]*)*"|'[^']*'|[^\s;&|"'\\])+|[;&|]+/).each do |word|
        break if word.start_with?("#")
        if word.match?(/\A[;&|]+\z/)
          sections << []
        else
          sections.last.concat(Shellwords.split(word))
        end
      end
      sections.reject(&:empty?)
    end
  end

  def strings(node)
    case node
    when Hash
      node.flat_map { |key, value| key == "run" && value.is_a?(String) ? commands(value).flatten : strings(value) }
    when Array then node.flat_map { |value| strings(value) }
    when String then [node]
    else []
    end
  end

  path = ARGV.fetch(0)
  workflow = YAML.load_file(path)
  inputs = strings(workflow)
  bad = []
  bad << "release workflow names __fault-points" if inputs.any? { |text| text.include?("__fault-points") }
  bad << "release workflow enables --all-features" if inputs.any? { |text| text.include?("--all-features") }
  workflow.fetch("jobs").each do |name, job|
    job.fetch("steps", []).each do |step|
      next unless step["run"]
      commands(step["run"]).each do |command|
        tool = command.index { |word| %w[cargo cross].include?(File.basename(word)) }
        next unless tool
        args = command.drop(tool + 1)
        build = args.index { |arg| %w[build b rustc zigbuild run r].include?(arg) }
        next unless build
        args = args.drop(build + 1).take_while { |arg| arg != "--" }
        packages = args.each_index.map do |i|
          case args[i]
          when "-p", "--package" then args[i + 1]
          when /\A--package=(.+)\z/, /\A-p(.+)\z/ then Regexp.last_match(1)
          end
        end.compact
        broad = args.any? { |arg| arg.match?(/\A--(?:workspace|all|all-targets|tests?|examples?|benches|bench)(?:=|\z)/) }
        if packages != ["ironflowx-bin"] || broad
          bad << "#{name}: release Cargo build must select only ironflowx-bin without dev targets"
        end
      end
    end
  end
  puts bad.uniq.map { |message| "#{path}: #{message}" }
  exit bad.empty?
RUBY

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
