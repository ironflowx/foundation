# Execution in release 0.4: claims, steps, replay, retries, leases and the
# fence, through Operations. Conventions, adapters and what is out of scope:
# README.md. The rules these scenarios sample: ../architecture.md, "Claim" to
# "Failure".
# @pending marks what is not built yet: the runner reports it, and fails if it passes.

Feature: Running a function step by step

  Background:
    Given a fresh engine
    And function "charge" with code "v1", no trigger and no policy
    And an executor with scope "execute"

  # ---- Claiming ----

  @EXE-001 @release-0.4
  Scenario: A run is queued, claimed, and ends with its output
    Given a run of "charge" was started
    And it reads as queued, due at once, created by the key that started it
    When the executor claims it and completes it with output {"ok": true}
    Then the run reads as completed with output {"ok": true}, 0 steps and execution 1

  @EXE-002 @release-0.4
  Scenario: A claimed run is not claimed again while its lease holds
    Given a run of "charge" was started and the executor claimed it
    When another executor claims without waiting
    Then its claim is empty
    And the run reads as running with a lease deadline

  @EXE-003 @release-0.4
  Scenario: A run stays on the code it was created with
    Given a run of "charge" was started
    And the builder put "charge" with code "v2" and started another run
    When an executor of "charge@v2" claims without waiting, twice
    Then the first claim is the second run, and the second claim is empty
    And an executor of "charge@v1" claims the first run

  @EXE-004 @release-0.4
  Scenario: A claim returns only functions it names
    Given a run of "charge" was started
    When the executor claims "refund@v1" without waiting
    Then its claim is empty

  @EXE-005 @release-0.4 @pending
  Scenario: The earliest due run is claimed first
    Given runs 1, 2 and 3 of "charge" were started in that order
    When the executor claims three times
    Then it gets runs 1, 2 and 3

  @EXE-006 @release-0.4 @pending
  Scenario: A waiting claim returns when a run is created
    Given the executor is waiting on a claim of "charge"
    When the builder starts "charge"
    Then the claim returns that run

  @EXE-007 @release-0.4 @pending
  Scenario: A claim that waits for nothing comes back empty
    When the executor claims "charge" and waits 1 second
    Then its claim is empty

  @EXE-008 @release-0.4 @pending
  Scenario Outline: A claim outside the rules is refused
    When the executor claims with <what>
    Then it is refused as invalid

    Examples:
      | what                  |
      | no function           |
      | 101 functions         |
      | the function "charge" |
      | a lease of 9 seconds  |
      | a lease of 901 seconds |
      | a wait of 21 seconds  |

  # ---- Steps and replay ----

  @EXE-010 @release-0.4
  Scenario: A recorded step is not run again
    Given an execution completed step 1 "reserve" with output {"id":  9007199254740993} and then released its run
    When another execution begins step 1 "reserve"
    Then the engine answers completed, with {"id":  9007199254740993} byte for byte
    And beginning step 2 "pay" answers run

  @EXE-011 @release-0.4 @pending
  Scenario: A step left open runs again in the next execution
    Given an execution began step 1 "reserve" and released its run
    When another execution begins step 1 "reserve"
    Then the engine answers run

  @EXE-012 @release-0.4 @pending
  Scenario: One name may be used for many steps
    When an execution completes steps 1, 2 and 3, each named "fetch", with outputs 10, 20 and 30
    And another execution replays them
    Then it is answered 10, 20 and 30

  @EXE-013 @release-0.4 @pending
  Scenario: The first outcome of a step stands
    Given an execution completed step 1 "reserve" with output 1
    When it completes step 1 with output 2
    And it fails step 1
    Then both answers are completed with output 1

  @EXE-014 @release-0.4 @pending
  Scenario: A changed step order fails the run at once
    Given an execution completed steps 1 "reserve" and 2 "pay" and released its run
    When another execution begins step 1 "pay"
    Then the engine answers ended
    And the run reads as failed with "ironflowx.step_order", naming position 1, "reserve" and "pay"

  @EXE-015 @release-0.4 @pending
  Scenario: Code that skips a recorded step cannot complete
    Given an execution completed steps 1 "reserve" and 2 "pay" and released its run
    When another execution replays step 1 and completes the run after 1 step
    Then the engine answers ended
    And the run reads as failed with "ironflowx.step_order"

  @EXE-016 @release-0.4 @pending
  Scenario: One step at a time
    Given an execution began step 1 "reserve"
    When it begins step 2 "pay"
    Then it is refused as invalid
    And completing the run is refused as invalid
    And completing step 1 with output 1 is answered completed

  @EXE-017 @release-0.4 @pending
  Scenario Outline: A position outside the run is refused
    Given an execution completed step 1 "reserve"
    When it <call>
    Then it is refused as invalid

    Examples:
      | call                   |
      | begins step 0 "x"      |
      | begins step 3 "x"      |
      | completes step 2       |
      | fails step 2           |

  @EXE-018 @release-0.4 @pending
  Scenario: A once step that may have run is not begun again
    Given an execution began step 1 "email" as once and released its run
    When another execution begins step 1 "email"
    Then the engine answers failed with "ironflowx.unknown_outcome"
    And beginning step 2 "check" answers run
    And a third execution, after a release, is answered the same failure for step 1

  @EXE-019 @release-0.4 @pending
  Scenario: A once step's failure is final
    Given an execution began step 1 "email" as once
    When it fails step 1 with a failure that is not final
    Then the engine answers failed with that error, and the execution goes on

  # ---- Failure and retries ----

  @EXE-020 @release-0.4 @pending
  Scenario: A failed step is retried later, and then goes on
    Given an execution completed step 1 "reserve" and failed step 2 "pay"
    Then the engine answered retry with a due time about 1 second later
    And the run reads as queued with that due time and the failure as its error
    When the run is due and another execution replays step 1 and begins step 2 "pay"
    Then step 1 is answered completed, and step 2 is answered run, attempt 2

  @EXE-021 @release-0.4 @pending
  Scenario: A failure outside any step retries the run on its own count
    Given the execution failed the run outside any step
    Then the engine answered retry
    When the run is due and another execution completes it
    Then the run reads as completed with 1 body failure

  @EXE-022 @release-0.4 @local @pending
  Scenario: Delays double up to the longest, give or take a tenth
    Given function "charge" with first delay 2 seconds and longest delay 10 seconds
    When step 1 fails 4 times, each once its run is due
    Then the delays were 2, 4, 8 and 10 seconds, each within 10 percent
    And the run cannot be claimed one second before its due time less that tenth

  @EXE-023 @release-0.4 @pending
  Scenario: A step's last failure is an outcome the code may handle
    Given function "charge" with 2 attempts
    And step 1 "pay" failed once and its run came due
    When the next execution fails step 1 again
    Then the engine answers failed with that error, and the execution goes on
    And completing step 2 "notify" and the run succeeds
    And the run reads as completed

  @EXE-024 @release-0.4 @pending
  Scenario: A final failure is not retried
    When the execution fails step 1 "pay" with a final failure
    Then the engine answers failed with that error
    When it fails the run with a final failure
    Then the run reads as failed with execution 1

  @EXE-025 @release-0.4 @pending
  Scenario: A run out of attempts fails
    Given function "charge" with 2 attempts
    And the run failed outside any step once and came due
    When the next execution fails the run outside any step
    Then the engine answers ended
    And the run reads as failed with that error

  @EXE-026 @release-0.4 @pending
  Scenario: A failure can ask for a longer wait
    When the execution fails step 1 "pay", asking to retry after 3600 seconds
    Then the engine answers retry with a due time 3600 seconds later

  @EXE-027 @release-0.4 @pending
  Scenario: A new policy reaches a run in flight
    Given function "charge" with 1 attempt, and a run of it was claimed
    And the builder put "charge" with code "v1" and 3 attempts
    When the execution fails step 1 "pay"
    Then the engine answers retry

  @EXE-028 @release-0.4 @pending
  Scenario: Steps keep their own count
    Given function "charge" with 2 attempts
    When step 1 "a" fails once and then completes, and step 2 "b" fails once
    Then the engine answered retry for step 2

  # ---- Leases and the fence ----

  @EXE-030 @release-0.4 @local
  Scenario Outline: A superseded execution changes nothing
    Given an execution of a 10 second lease began step 1 "pay"
    And its deadline passed and another executor claimed the run
    When the first execution <call>
    Then it is refused as a conflict, with a hint to stop
    And the run is as it was

    Examples:
      | call              |
      | begins step 1     |
      | completes step 1  |
      | completes the run |
      | releases the run  |

    @pending
    Examples:
      | call              |
      | fails step 1      |
      | fails the run     |

  @EXE-031 @release-0.4
  Scenario: A lease belongs to the key that claimed it
    Given the executor claimed a run
    When an executor with another key completes that run
    Then it is refused as a conflict
    And the run reads as running

  @EXE-032 @release-0.4 @local
  Scenario: A heartbeat renews the leases it names
    Given the executor claimed runs 1 and 2 with 10 second leases
    When 8 seconds pass, and it heartbeats run 1 only, and 4 seconds pass
    And another executor claims without waiting, twice
    Then the first claim is run 2, and the second claim is empty

  @EXE-033 @release-0.4
  Scenario: A heartbeat answers each lease on its own
    Given the executor claimed run 1 and released run 2
    When it heartbeats both
    Then run 1 is renewed and run 2 is a conflict

  @EXE-034 @release-0.4 @local @pending
  Scenario: A lapsed lease is claimed by another executor and spends no attempt
    Given function "charge" with 1 attempt
    And the executor claimed a run with a 10 second lease and began step 1 "pay"
    When 10 seconds pass and another executor claims
    Then it gets that run, execution 2, and beginning step 1 "pay" answers run
    And the run reads as having lost 1 lease

  @EXE-035 @release-0.4 @local
  Scenario: A run that keeps losing its executor fails
    Given a run of "charge" lost 9 leases in a row with no step outcome
    When the tenth lapses and an executor claims without waiting
    Then its claim is empty
    And the run reads as failed with "ironflowx.executor_lost"

  @EXE-036 @release-0.4 @pending
  Scenario Outline: A call whose answer was lost is repeated safely
    Given an execution that <did>
    When it sends the same call again
    Then it gets the same answer, and nothing new is recorded

    Examples:
      | did                                   |
      | began step 1 "pay"                    |
      | began step 1 "pay" as once            |
      | completed step 1 "pay"                |
      | failed step 1 "pay" to be retried     |
      | failed step 1 "pay" with a final failure |
      | completed the run                     |
      | failed the run to be retried          |
      | failed the run with a final failure   |
      | released the run                      |

  @EXE-037 @release-0.4 @local
  Scenario: A late executor keeps a run nobody took
    Given an execution of a 10 second lease began step 1 "pay"
    When 30 seconds pass and it completes step 1 with output 1
    Then the engine answers completed
    And its lease has a new deadline

  @EXE-038 @release-0.4
  Scenario: A released run is claimed at once and nothing is counted
    Given the executor claimed a run and released it
    When another executor claims without waiting
    Then it gets that run, execution 2
    And the run reads as having lost no lease and failed no attempt

  @EXE-039 @release-0.4
  Scenario Outline: A lease that names nothing is not found
    When the executor completes the run of lease <lease>
    Then it is refused as not found

    Examples:
      | lease                             |
      | built: an unknown run, execution 1 |
      | "not-a-lease"                     |

  # ---- The end of a run ----

  @EXE-040 @release-0.4 @pending
  Scenario Outline: A run's end is an event
    When the executor claims a run of "charge" and <ends> it
    Then listing events shows one "<event>" that names the run, "charge" and "v1", emitted by the executor's key
    And its data <error>

    Examples:
      | ends                              | event                   | error                        |
      | completes                         | ironflowx.run.completed | holds no error and no output |
      | fails, with a final failure       | ironflowx.run.failed    | holds the error              |

  @EXE-041 @release-0.4 @pending
  Scenario Outline: An ended run never changes
    Given the execution completed its run with output 1
    When it <call>
    Then the engine answers ended, completed
    And the run reads as completed with output 1, and has one end event

    Examples:
      | call                          |
      | begins step 1 "pay"           |
      | completes the run with output 2 |
      | fails the run                 |

  @EXE-042 @release-0.4 @pending
  Scenario: An executor cannot use the engine's error codes
    When the execution fails step 1 "pay" with code "ironflowx.executor_lost"
    Then it is refused as invalid
    And step 1 is still open

  @EXE-043 @release-0.4 @pending
  Scenario Outline: Outputs follow the event-data rules
    When the execution completes <what> with <output>
    Then it is refused as invalid

    Examples:
      | what    | output                               |
      | a step  | built: JSON of 524,289 bytes         |
      | a step  | {"a": 1, "a": 2}                     |
      | a step  | {"$redacted": true}                  |
      | the run | built: JSON of 524,289 bytes         |
      | the run | built: text that is not JSON         |

  @EXE-044 @release-0.4 @pending
  Scenario: A run has at most 1,000 steps
    Given an execution completed 1,000 steps
    When it begins step 1001 "one-more"
    Then the engine answers ended
    And the run reads as failed with "ironflowx.too_many_steps"

  @EXE-045 @release-0.4 @pending
  Scenario: Code that skips a step left open cannot complete
    Given an execution completed step 1 "reserve", began step 2 "pay" and released its run
    When another execution replays step 1 and completes the run after 1 step
    Then the engine answers ended
    And the run reads as failed with "ironflowx.step_order"

  @EXE-046 @release-0.4 @pending
  Scenario Outline: A run can fail with a step's recorded error
    Given step 1 "email" failed with <error>
    When the execution fails the run, naming step 1 as the failed step
    Then the engine answers ended
    And the run reads as failed with that error and step 1 "email", and no body failure
    And its end event holds that error

    Examples:
      | error                                       |
      | a final failure of the executor's own       |
      | "ironflowx.unknown_outcome" of a once step  |

  @EXE-047 @release-0.4 @pending
  Scenario Outline: Only a step that failed can be named
    Given an execution completed step 1 "reserve"
    When it fails the run, naming <step>
    Then it is refused as invalid

    Examples:
      | step                              |
      | step 1 as the failed step         |
      | step 2 as the failed step         |
      | step 1 and a message              |

  # ---- Reading runs ----

  @EXE-050 @release-0.4 @pending
  Scenario: Runs list newest first, filtered by function, status and event
    Given runs of "charge" and "refund", some completed, some queued, two created by one event
    When the builder lists runs by function, by status, by event, and by all three
    Then each page holds exactly the runs that match, newest first, without inputs or outputs

  @EXE-051 @release-0.4 @pending
  Scenario: An unknown run is not found
    When the builder gets a run that does not exist
    Then it is refused as not found

  @EXE-052 @release-0.4 @local @pending
  Scenario: An engine restart keeps every run where it was
    Given a run with a lease of 60 seconds and step 1 "pay" open, and a run due in 5 seconds
    When the engine is closed and opened again
    Then the first run's execution completes step 1 and the run
    And once 5 seconds have passed the second run is claimed

  @EXE-053 @release-0.4 @local @pending
  Scenario: A clock set back does not hide a due run
    Given the clock was set an hour ahead, an event was emitted, and the clock was set back
    When the builder starts "charge" and the executor claims without waiting
    Then it gets that run

  @EXE-054 @release-0.4 @local @pending
  Scenario: An engine that was down longer than a lease takes no run from its executor
    Given the executor claimed a run with a 10 second lease and began step 1 "email" as once
    When the engine is closed, 60 seconds pass, and it is opened again
    And another executor claims without waiting
    Then its claim is empty
    And the first executor completes step 1 and the run, in execution 1
