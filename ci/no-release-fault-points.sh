#!/usr/bin/env bash
# Usage: ci/no-release-fault-points.sh [release.yml] [Cargo.toml]
# Archive builds use an explicit, package-only Cargo command in release.yml. A workspace build or
# test build can unify in the hooks from dev-dependencies or the crash-test crate. Commands hidden
# in another script or constructed from shell variables are outside this check; keep archive build
# arguments literal in release.yml.
set -euo pipefail

workflow=${1:-.github/workflows/release.yml}
ruby -ryaml - "$workflow" <<'RUBY'
  SHELL_PREFIXES = %w[if then elif else do ! time exec command nohup {].freeze
  ENV_VALUE_FLAGS = %w[-u --unset -C --chdir -S --split-string].freeze
  CARGO_GLOBAL_VALUE_FLAGS = %w[-C --color --config -Z --explain].freeze
  DESCRIPTIVE_KEYS = %w[name description].freeze

  # Extract the inner body of $(...) starting at '('. Returns [inner, next_index].
  def extract_parens(script, open_idx)
    i = open_idx + 1
    depth = 1
    in_single = false
    in_double = false
    while i < script.length && depth > 0
      ch = script[i]
      if in_single
        in_single = false if ch == "'"
      elsif in_double
        if ch == "\\" && i + 1 < script.length
          i += 1
        elsif ch == '"'
          in_double = false
        end
      elsif ch == "\\" && i + 1 < script.length
        i += 1
      elsif ch == "'"
        in_single = true
      elsif ch == '"'
        in_double = true
      elsif ch == "("
        depth += 1
      elsif ch == ")"
        depth -= 1
        return [script[(open_idx + 1)...i], i + 1] if depth == 0
      end
      i += 1
    end
    raise ArgumentError, "unterminated $("
  end

  # Extract the inner body of `...` starting at the opening backtick. Returns [inner, next_index].
  def extract_backticks(script, open_idx)
    i = open_idx + 1
    inner = +""
    while i < script.length
      ch = script[i]
      if ch == "\\" && i + 1 < script.length
        nxt = script[i + 1]
        if nxt == "\n"
          i += 2
          next
        elsif nxt == "\r" && script[i + 2] == "\n"
          i += 3
          next
        elsif %w[` $ \\].include?(nxt)
          inner << nxt
          i += 2
          next
        end
      elsif ch == "`"
        return [inner, i + 1]
      end
      inner << ch
      i += 1
    end
    raise ArgumentError, "unterminated backtick"
  end

  # Tokenize literal shell commands, including subshells and command substitutions, without
  # evaluating variables. Backslash-newline continuations outside single quotes join tokens.
  def commands(script)
    out = []
    current = []
    word = +""
    in_word = false
    flush_word = lambda do
      if in_word
        current << word
        word = +""
        in_word = false
      end
    end
    flush_cmd = lambda do
      flush_word.call
      unless current.empty?
        out << current
        current = []
      end
    end

    i = 0
    n = script.length
    while i < n
      ch = script[i]
      if ch == "\\" && i + 1 < n
        if script[i + 1] == "\n"
          i += 2
        elsif script[i + 1, 2] == "\r\n"
          i += 3
        else
          in_word = true
          word << script[i + 1]
          i += 2
        end
      elsif ch == "'"
        in_word = true
        i += 1
        close = script.index("'", i)
        raise ArgumentError, "unterminated single quote" unless close
        word << script[i...close]
        i = close + 1
      elsif ch == '"'
        in_word = true
        i += 1
        closed = false
        while i < n
          c = script[i]
          if c == "\\" && i + 1 < n
            nxt = script[i + 1]
            if nxt == "\n"
              i += 2
            elsif nxt == "\r" && script[i + 2] == "\n"
              i += 3
            elsif %w[" \\ $ `].include?(nxt)
              word << nxt
              i += 2
            else
              word << c << nxt
              i += 2
            end
          elsif c == '"'
            i += 1
            closed = true
            break
          elsif c == "$" && script[i + 1] == "("
            inner, i = extract_parens(script, i + 1)
            out.concat(commands(inner))
          elsif c == "`"
            inner, i = extract_backticks(script, i)
            out.concat(commands(inner))
          else
            word << c
            i += 1
          end
        end
        raise ArgumentError, "unterminated double quote" unless closed
      elsif ch == "#" && !in_word
        nl = script.index("\n", i)
        i = nl || n
      elsif ch == "$" && script[i + 1] == "("
        in_word = true
        inner, i = extract_parens(script, i + 1)
        out.concat(commands(inner))
      elsif ch == "`"
        in_word = true
        inner, i = extract_backticks(script, i)
        out.concat(commands(inner))
      elsif ch == " " || ch == "\t" || ch == "\r"
        flush_word.call
        i += 1
      elsif "\n;&|()".include?(ch)
        flush_cmd.call
        i += 1
      else
        in_word = true
        word << ch
        i += 1
      end
    end
    flush_cmd.call
    out
  end

  def split_command(tokens)
    env = []
    i = 0
    while i < tokens.length
      tok = tokens[i]
      if tok.match?(/\A[A-Za-z_][A-Za-z0-9_]*=/)
        env << tok
        i += 1
      elsif SHELL_PREFIXES.include?(tok)
        i += 1
      elsif File.basename(tok) == "env"
        i += 1
        while i < tokens.length
          if tokens[i] == "--"
            i += 1
            break
          elsif ENV_VALUE_FLAGS.include?(tokens[i])
            i += 2
          elsif tokens[i].start_with?("-")
            i += 1
          elsif tokens[i].match?(/\A[A-Za-z_][A-Za-z0-9_]*=/)
            env << tokens[i]
            i += 1
          else
            break
          end
        end
      else
        break
      end
    end
    [env, tokens.drop(i)]
  end

  def find_subcommand(args)
    i = 0
    while i < args.length
      arg = args[i]
      if arg.start_with?("+")
        i += 1
      elsif CARGO_GLOBAL_VALUE_FLAGS.include?(arg)
        i += 2
      elsif arg.start_with?("-")
        i += 1
      else
        return [arg, i]
      end
    end
    nil
  end

  def executable_strings(node)
    case node
    when Hash
      node.flat_map do |key, value|
        next [] if key == "run" || DESCRIPTIVE_KEYS.include?(key)
        executable_strings(value)
      end
    when Array then node.flat_map { |value| executable_strings(value) }
    when String then [node]
    else []
    end
  end

  def check_tokens(tokens, label, bad)
    if tokens.any? { |text| text.include?("__fault-points") }
      bad << "#{label}: release workflow names __fault-points"
    end
    if tokens.any? { |text| text.include?("--all-features") }
      bad << "#{label}: release workflow enables --all-features"
    end
  end

  path = ARGV.fetch(0)
  workflow = YAML.load_file(path)
  bad = []
  top_strings = executable_strings(workflow.reject { |key, _| key == "jobs" })
  check_tokens(top_strings, "workflow", bad)

  workflow.fetch("jobs").each do |name, job|
    check_tokens(executable_strings(job), name, bad)
    job.fetch("steps", []).each do |step|
      next unless step["run"].is_a?(String)
      commands(step["run"]).each do |tokens|
        env, cmd = split_command(tokens)
        check_tokens(env, name, bad)
        next if cmd.empty?
        exe = File.basename(cmd.first)
        args = cmd.drop(1)
        if %w[cargo cross].include?(exe)
          cargo_args = args.take_while { |arg| arg != "--" }
          check_tokens(cargo_args, name, bad)
          sub, sub_idx = find_subcommand(cargo_args)
          next unless sub
          if %w[test t bench].include?(sub)
            bad << "#{name}: release Cargo build must select only ironflowx-bin without dev targets"
            next
          end
          next unless %w[build b rustc zigbuild run r].include?(sub)
          sub_args = cargo_args.drop(sub_idx + 1)
          packages = sub_args.each_index.map do |i|
            case sub_args[i]
            when "-p", "--package" then sub_args[i + 1]
            when /\A--package=(.+)\z/, /\A-p(.+)\z/ then Regexp.last_match(1)
            end
          end.compact
          broad = sub_args.any? do |arg|
            arg.match?(/\A--(?:workspace|all|all-targets|tests?|examples?|benches|bench)(?:=|\z)/)
          end
          if packages != ["ironflowx-bin"] || broad
            bad << "#{name}: release Cargo build must select only ironflowx-bin without dev targets"
          end
        else
          check_tokens(cmd, name, bad)
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
