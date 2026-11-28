# Functions in release 0.4: putting a definition, and the runs an event creates.
# Conventions, adapters and what is out of scope: README.md. The rules these
# scenarios sample: ../architecture.md, "Functions" and "Creating runs".
# @pending marks what is not built yet: the runner reports it, and fails if it passes.

Feature: Functions and triggers

  Background:
    Given a fresh engine
    And a builder with scopes "read" and "write"

  # ---- Definitions ----

  @FUN-001 @release-0.4
  Scenario: A function reads back as it was put
    When the builder puts function "charge" with code "v1", trigger "order.placed" and 5 attempts
    Then the put reports a change
    And getting "charge" shows code "v1", trigger "order.placed", 5 attempts, no delays, and the builder's key as who put it

  @FUN-002 @release-0.4
  Scenario: Putting the same definition again changes nothing
    Given the builder put function "charge" with code "v1" and trigger "order.placed"
    When the builder puts the same definition again
    Then the put reports no change
    And getting "charge" shows the first put's time

  @FUN-003 @release-0.4
  Scenario: A put replaces the whole definition
    Given the builder put function "charge" with code "v1", trigger "order.placed" and 5 attempts
    When the builder puts function "charge" with code "v2" and no trigger
    And a client emits "order.placed"
    Then getting "charge" shows code "v2", no trigger and no number of attempts
    And "charge" has no run

  @FUN-004 @release-0.4
  Scenario Outline: A definition outside the rules is refused
    When the builder puts a function with <what>
    Then it is refused as invalid

    Examples:
      | what                                  |
      | an empty name                         |
      | a name of 129 bytes                   |
      | the name "a@b"                        |
      | an empty code label                   |
      | 11 triggers                           |
      | the trigger "order.placed" twice      |
      | the trigger "order..placed"           |
      | 0 attempts                            |
      | 101 attempts                          |
      | a first delay above its longest delay |

  @FUN-005 @release-0.4
  Scenario Outline: The engine's own events trigger nothing
    When the builder puts a function with the trigger "<event>"
    Then it is refused as invalid

    Examples:
      | event                   |
      | ironflowx.run.failed    |
      | ironflowx.run.completed |
      | $system.anything        |

  @FUN-006 @release-0.4
  Scenario: At most 20 functions wait for one event
    Given 20 functions are triggered by "order.placed"
    When the builder puts another function triggered by "order.placed"
    Then it is refused as resource exhausted, with a hint that names "order.placed"
    And putting one of the 20 again with new code succeeds

  @FUN-007 @release-0.4
  Scenario: Functions list by name, page by page
    Given functions "c", "a" and "b" were put
    When the builder lists functions 2 at a time
    Then the pages are "a", "b" and then "c", with no page after it

  @FUN-008 @release-0.4
  Scenario Outline: Each call needs its scope and no other
    Given a key with every scope but "<scope>"
    When the key calls <operation>
    Then it is refused as permission denied, with a hint that names "<scope>"

    Examples:
      | operation       | scope   |
      | put a function  | write   |
      | get a function  | read    |
      | start a run     | write   |
      | get a run       | read    |
      | claim a run     | execute |
      | begin a step    | execute |
      | complete a step | execute |
      | complete a run  | execute |
      | list functions  | read    |

    @pending
    Examples:
      | operation       | scope   |
      | list runs       | read    |
      | heartbeat       | execute |
      | release a run   | execute |
      | fail a step     | execute |
      | fail a run      | execute |

  # ---- Triggers ----

  @FUN-010 @release-0.4 @pending
  Scenario: One event creates one run for each function it triggers
    Given functions "charge" and "notify" are triggered by "order.placed", and "audit" by "order.paid"
    When a client emits "order.placed"
    Then listing runs of that event shows one queued run of "charge" and one of "notify"

  @FUN-011 @release-0.4 @pending
  Scenario: Emitting an event again by its key creates no run
    Given function "charge" is triggered by "order.placed"
    And a client emitted "order.placed" with idempotency key "k-1"
    When the client emits "order.placed" with idempotency key "k-1" again
    Then the answer is a duplicate
    And "charge" has 1 run

  @FUN-012 @release-0.4 @pending
  Scenario: A batch creates runs for the events it stored
    Given function "charge" is triggered by "order.placed"
    When a client emits a batch of "order.placed", an invalid event, and "order.placed"
    Then "charge" has 2 runs, one for each stored event

  @FUN-013 @release-0.4 @pending
  Scenario: A function does not see events from before its trigger
    Given a client emitted "order.placed"
    When the builder puts function "charge" triggered by "order.placed"
    Then "charge" has no run

  @FUN-014 @release-0.4 @pending
  Scenario: A triggered run's claim carries the whole event
    Given function "charge" is triggered by "order.placed"
    And a client emitted "order.placed" with data {"id": 12345678901234567890}, metadata {"trace": "t-1"}, version 3 and idempotency key "k-1"
    When an executor claims a run of "charge"
    Then the claim holds that event, with its ID, data as sent, metadata, version, key, time and emitter

  # ---- Starting a run ----

  @FUN-020 @release-0.4 @pending
  Scenario: A run started directly carries its input and stores no event
    Given function "charge" with no trigger
    When the builder starts "charge" with input {"order": 7}
    Then a queued run of "charge" exists with input {"order": 7} and no event
    And listing events shows none

  @FUN-021 @release-0.4 @pending
  Scenario: Starting again with the same key answers the first run
    Given function "charge" with no trigger
    And the builder started "charge" with key "k-1"
    When the builder starts "charge" with key "k-1" and another input
    Then the answer is the first run and a duplicate

  @FUN-022 @release-0.4 @pending
  Scenario: A start key names one function
    Given functions "charge" and "refund" with no trigger
    And the builder started "charge" with key "k-1"
    When the builder starts "refund" with key "k-1"
    Then it is refused as a conflict
    And "refund" has no run

  @FUN-023 @release-0.4 @pending
  Scenario: Start keys and event keys do not meet
    Given function "charge" with no trigger
    And a client emitted "order.placed" with idempotency key "k-1"
    When the builder starts "charge" with key "k-1"
    Then the answer is a new run

  @FUN-024 @release-0.4 @pending
  Scenario: An unknown function cannot be started
    When the builder starts "nobody"
    Then it is refused as not found

  @FUN-025 @release-0.4 @pending
  Scenario Outline: A run input follows the event-data rules
    Given function "charge" with no trigger
    When the builder starts "charge" with input <input>
    Then <answer>

    Examples:
      | input                             | answer                |
      | built: none                       | the run's input is null |
      | built: JSON of 524,288 bytes      | a run is created      |
      | built: JSON of 524,289 bytes      | it is refused as invalid |
      | {"a": 1, "a": 2}                  | it is refused as invalid |
