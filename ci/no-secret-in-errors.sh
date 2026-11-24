#!/usr/bin/env bash
# Usage: ci/no-secret-in-errors.sh <rustdoc JSON...>
# TEN-3: fails if an error type (one with an `impl Error`, or named *Error, *Refused, *Malformed)
# can hold a Secret: in a field or variant, private ones included, of its own or of a type it
# holds, however deeply, through generics and their defaults, aliases and associated types, or
# because a signature, a field, an alias or an associated type uses it with one, `Self` included,
# also by way of the parameters of a generic type, its methods or an alias. The JSON, of every
# crate between Secret and the errors, is built with --document-private-items and
# --document-hidden-items; rustdoc has resolved the names and expanded the macros. A function
# pointer that takes a Secret holds none.
# It refuses too much, so fails closed, for PhantomData<Secret>, a `dyn` callable that takes a
# Secret, a generic type one of whose defaults holds one, whatever it is used with, an associated
# type of a trait any type implements with one, a generic error used with one it does not store,
# and one that only an impl for other arguments uses.
# It does not see:
# - a type declared in the body of a function, a constant or a static;
# - a generic parameter's default on a trait;
# - an alias, or a re-export under a new name, that alone gives a type an error's name;
# - a Secret a generic error is used with only in a body, or by way of a parameter of a function or
#   a trait (a trait's `Self` too), or of one an impl names inside another type (`impl A<Vec<T>>`).
# shortcut: what a `dyn Trait` field hides is not seen; upgrade if an error ever stores one.
set -euo pipefail
# shellcheck disable=SC2016 # ruby's own `#{}`
exec ruby -rjson -e '
  SECRET = "ironflowx_core::secret::Secret"
  ERROR = /(Error|Refused|Malformed)\z/
  holds = Hash.new { |h, k| h[k] = [] }   # a type => the types its fields, or its alias, name
  errors = {}                             # an error type => the types its impl, or a use, names
  uses = []                               # a type anything names, and its arguments
  # A type => its parameters and their defaults, and what it names: its fields, or its alias.
  generics = {}
  impls = Hash.new { |h, k| h[k] = [] }   # a type => its arguments in an impl, and what that names
  # Every type a type names. Not what a function pointer takes or returns.
  types = lambda do |node|
    case node
    when Array then node.flat_map(&types)
    when Hash
      node.flat_map do |key, value|
        next [] if key == "function_pointer"
        own = []
        own << value["id"] if key == "resolved_path"
        own << value["trait"]["id"] if key == "qualified_path" && value["trait"]
        own + types.(value)
      end
    else []
    end
  end
  # Every type named with its arguments.
  applied = lambda do |node|
    case node
    when Array then node.flat_map(&applied)
    when Hash
      used = node["resolved_path"]
      (used ? [[used["id"], used["args"]]] : []) + node.values.flat_map(&applied)
    else []
    end
  end
  # The types among the arguments.
  given = ->(args) { (args&.dig("angle_bracketed", "args") || []).grep(Hash).map { |a| a["type"] }.compact }
  # A type with each generic parameter in `by` replaced.
  put = lambda do |node, by|
    case node
    when Array then node.map { |n| put.(n, by) }
    when Hash
      node["generic"] && by[node["generic"]] || node.transform_values { |v| put.(v, by) }
    else node
    end
  end
  ARGV.each do |file|
    doc = JSON.parse(File.read(file))
    abort "#{file} was built without --document-private-items" unless doc["includes_private"]
    name = lambda do |id|
      known = doc["paths"][id.to_s]
      known ? known["path"].join("::") : "#{doc["index"][doc["root"].to_s]["name"]}##{id}"
    end
    # The ID of a type or a trait, which is of this file alone, becomes its name.
    named = lambda do |node|
      case node
      when Array then node.map(&named)
      when Hash
        node = node.transform_values(&named)
        node["path"] && node["id"].is_a?(Integer) ? node.merge("id" => name.(node["id"])) : node
      else node
      end
    end
    doc["index"].each_value { |item| item["inner"] = named.(item["inner"]) }
    inner = ->(id) { doc["index"][id.to_s] && doc["index"][id.to_s]["inner"] }
    # The fields of a struct, a variant or a union, and the variants of an enum, by ID.
    ids = lambda do |node|
      case node
      when Integer then [node]
      when Array then node.flat_map(&ids)
      when Hash then node.reject { |k, _| %w[impls generics discriminant].include?(k) }.values.flat_map(&ids)
      else []
      end
    end
    # The types of those fields.
    fields = lambda do |inner_of|
      kind, body = inner_of.first
      case kind
      when "struct", "union", "enum", "variant"
        ids.(body).map(&inner).compact.flat_map(&fields)
      when "struct_field" then [body]
      else []
      end
    end
    doc["index"].each do |id, item|
      kind, body = item["inner"].first
      uses.concat(applied.(body))
      type = name.(id)
      case kind
      when "struct", "union", "enum", "type_alias"
        nodes = kind == "type_alias" ? [body["type"]] : fields.(item["inner"])
        params = body["generics"]["params"].select { |p| p["kind"]["type"] }
        params = params.map { |p| [p["name"], p["kind"]["type"]["default"]] }
        generics[type] = [params, nodes]
        holds[type].concat(types.(nodes) + types.(params))
        errors[type] ||= [] if kind != "type_alias" && item["name"] =~ ERROR
      when "impl"
        items = body["items"].map(&inner).compact
        uses.concat(applied.(put.(items, "Self" => body["for"])))
        self_path = body["for"]["resolved_path"]
        impls[self_path["id"]] << [given.(self_path["args"]), items] if self_path
        next unless body["trait"]
        # An associated type: whoever names the trait may hold it.
        items.each { |i| holds[body["trait"]["id"]].concat(types.(i["assoc_type"]["type"])) if i["assoc_type"] }
        next if body["blanket_impl"] || body["trait"]["id"] != "core::error::Error"
        self_types = types.(body["for"])
        (errors[self_types.first] ||= []).concat(self_types) if self_types.any?
      end
    end
  end
  abort "no #{SECRET} in the JSON: is its core crate there, and its format read right?" unless holds.key?(SECRET)
  abort "no error type in the JSON: is its format read right?" if errors.empty?
  # An error used with a type may hold it: a use, and each it makes by what the type names.
  seen = {}
  while (type, args = uses.pop)
    args_given = given.(args)
    key = [type, args_given.map { |a| types.(a).uniq }]
    next if seen[key]
    seen[key] = true
    (errors[type] ||= []).concat(types.(args)) if errors.key?(type) || type =~ ERROR
    params, nodes = generics[type]
    next unless params
    itself = { "Self" => { "resolved_path" => { "id" => type, "args" => args } } }
    # A default may name the parameters before it.
    by = params.each_with_index.inject(itself) do |before, ((param, default), i)|
      arg = args_given[i] || put.(default, before)
      arg ? before.merge(param => arg) : before
    end
    uses.concat(applied.(put.(nodes, by)))
    impls[type].each do |impl_args, items|
      by = impl_args.zip(args_given).map { |own, arg| [own["generic"], arg] }.to_h
      uses.concat(applied.(put.(items, itself.merge(by))))
    end
  end
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
