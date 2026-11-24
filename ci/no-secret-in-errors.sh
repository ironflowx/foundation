#!/usr/bin/env bash
# Usage: ci/no-secret-in-errors.sh <rustdoc JSON...>
# TEN-3: fails if an error type (one with an `impl Error`, or named *Error, *Refused, *Malformed)
# can hold a Secret: in a field or variant, private ones included, of its own or of a type it
# holds, however deeply, through generics and their defaults, aliases and associated types. The
# JSON, of every crate between Secret and the errors, is built with --document-private-items and
# --document-hidden-items; rustdoc has resolved the names and expanded the macros. A function
# pointer that takes a Secret holds none.
# It refuses too much, so fails closed, for PhantomData<Secret>, a `dyn` callable that takes a
# Secret, a generic type one of whose defaults holds one, whatever it is used with, and an
# associated type of a trait any type implements with one. It does not see a type declared in a
# function body.
# shortcut: what a `dyn Trait` field hides is not seen; upgrade if an error ever stores one.
set -euo pipefail
# shellcheck disable=SC2016 # ruby's own `#{}`
exec ruby -rjson -e '
  SECRET = "ironflowx_core::secret::Secret"
  holds = Hash.new { |h, k| h[k] = [] }   # a type => the types its fields, or its alias, name
  errors = {}                             # an error type => the types its impl names
  ARGV.each do |file|
    doc = JSON.parse(File.read(file))
    abort "#{file} was built without --document-private-items" unless doc["includes_private"]
    path = ->(id) { doc["paths"][id.to_s] && doc["paths"][id.to_s]["path"].join("::") }
    name = lambda do |id|
      path.(id) || "#{doc["index"][doc["root"].to_s]["name"]}##{id}"
    end
    # Every type a type names. Not what a function pointer takes or returns.
    types = lambda do |node|
      case node
      when Array then node.flat_map(&types)
      when Hash
        node.flat_map do |key, value|
          next [] if key == "function_pointer"
          own = []
          own << name.(value["id"]) if key == "resolved_path"
          own << name.(value["trait"]["id"]) if key == "qualified_path" && value["trait"]
          own + types.(value)
        end
      else []
      end
    end
    # The fields of a struct, a variant or a union, and the variants of an enum, by ID.
    ids = lambda do |node|
      case node
      when Integer then [node]
      when Array then node.flat_map(&ids)
      when Hash then node.reject { |k, _| %w[impls generics discriminant].include?(k) }.values.flat_map(&ids)
      else []
      end
    end
    fields = lambda do |item|
      kind = item["inner"].keys.first
      case kind
      when "struct", "union", "enum", "variant"
        ids.(item["inner"][kind]).flat_map { |i| (f = doc["index"][i.to_s]) ? fields.(f) : [] }
      when "struct_field" then types.(item["inner"][kind])
      else []
      end
    end
    # What its generic parameters are unless given.
    defaults = lambda do |body|
      types.(body["generics"]["params"].map { |p| p["kind"].dig("type", "default") })
    end
    doc["index"].each_value do |item|
      kind = item["inner"].keys.first
      body = item["inner"][kind]
      case kind
      when "struct", "union", "enum"
        holds[name.(item["id"])].concat(fields.(item) + defaults.(body))
        errors[name.(item["id"])] ||= [] if item["name"] =~ /(Error|Refused|Malformed)\z/
      when "type_alias"
        holds[name.(item["id"])].concat(types.(body["type"]) + defaults.(body))
      when "impl"
        next unless body["trait"]
        # An associated type: whoever names the trait may hold it.
        body["items"].each do |i|
          type = doc["index"][i.to_s] && doc["index"][i.to_s]["inner"]["assoc_type"]
          holds[name.(body["trait"]["id"])].concat(types.(type["type"])) if type
        end
        next if body["blanket_impl"] || path.(body["trait"]["id"]) != "core::error::Error"
        named = types.(body["for"])
        (errors[named.first] ||= []).concat(named) if named.any?
      end
    end
  end
  abort "no #{SECRET} in the JSON: is its core crate there, and its format read right?" unless holds.key?(SECRET)
  abort "no error type in the JSON: is its format read right?" if errors.empty?
  # To a fixed point.
  held = { SECRET => true }
  loop do
    more = holds.select { |type, names| !held[type] && names.any? { |n| held[n] } }
    break if more.empty?
    more.each_key { |type| held[type] = true }
  end
  leaks = errors.select { |type, named| ([type] + named).any? { |n| held[n] } }.keys.sort
  leaks.each { |type| puts "TEN-3: #{type} is an error type and can hold a Secret" }
  exit 1 if leaks.any?
' "$@"
