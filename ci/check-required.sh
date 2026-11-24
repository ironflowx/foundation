#!/usr/bin/env bash
# Usage: ci/check-required.sh <workflow> <exempt-job>... < <branch rules>
# Fails unless the status checks the branch rules require (GitHub's `rules/branches/<branch>`, as
# JSON) are exactly the jobs of <workflow>, each matrix value its own check, less the exempt jobs:
# a job left out, or that can be skipped, cannot block a merge, and a check without its job blocks
# every one.
set -euo pipefail
# shellcheck disable=SC2016 # ruby's own `#{}` and `$stdin`
exec ruby -ryaml -rjson -e '
  workflow, *exempt = ARGV
  jobs = YAML.load_file(workflow).fetch("jobs")
  bad = (exempt - jobs.keys).map { |id| "no job to exempt: #{id}" }
  named = jobs.reject { |id, _| exempt.include?(id) }.flat_map do |id, job|
    name = (job["name"] || id).to_s
    matrix = job.dig("strategy", "matrix") || {}
    values = matrix.is_a?(Hash) ? matrix.values : [nil]
    # What GitHub would call these is not decided by the file alone, or not simply.
    if job["uses"] || name.include?("${{") || (job["name"] && values.any?) || !values.all?(Array) ||
       matrix.key?("include") || matrix.key?("exclude")
      bad << "cannot tell the checks of: #{id}"
      next []
    end
    # GitHub counts a skipped job, and one whose failure is let go, as passed.
    bad << "can pass without passing: #{id}" if job.key?("if") || job["continue-on-error"]
    values.empty? ? [name] : values.first.product(*values.drop(1)).map { |v| "#{name} (#{v.join(", ")})" }
  end
  rules = JSON.parse($stdin.read).select { |r| r["type"] == "required_status_checks" }
  required = rules.flat_map { |r| r["parameters"]["required_status_checks"].map { |c| c["context"] } }
  bad += (named - required).map { |c| "not required: #{c}" }
  bad += (required - named).map { |c| "required, but no job of it: #{c}" }
  puts bad
  exit bad.empty?
' "$@"
