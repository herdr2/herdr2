import { describe, expect, test } from "bun:test";
import { setLang, translate } from "../src/i18n";
import { reviewCircuitView, taskRequiresReview } from "../src/review";
import { buildCoordinatorAttention, countTasksByFilter, taskMatchesFilter } from "../src/workbench";
import type { ReviewCircuit, RunSnapshot, Task } from "../src/types";

function makeTask(overrides: Partial<Task> = {}): Task {
  return {
    id: overrides.id ?? "tsk_1",
    key: overrides.key ?? "final-review",
    title: overrides.title ?? "Final Review",
    status: overrides.status ?? "ready",
    deps: overrides.deps ?? [],
    tags: overrides.tags ?? [],
    files: overrides.files ?? [],
    isolation: overrides.isolation ?? "none",
    spec: overrides.spec ?? { target: "workspace", change: "review", acceptance: "approved" },
    ...overrides,
  };
}

function makeSnapshot(tasks: Task[]): RunSnapshot {
  return {
    run: {
      id: "run_1",
      objective: "Review circuit test",
      cwd: "/tmp/repo",
      host: "herdr2",
      hostSession: "s1",
      maxConcurrency: 2,
      status: "active",
      createdAt: "2026-11-24T00:00:00Z",
      updatedAt: "2026-11-24T00:00:00Z",
    },
    tasks,
    workers: [],
    attempts: [],
    pendingMessages: [],
    messages: [],
    stalls: [],
    stats: {
      total: tasks.length,
      succeeded: 0,
      failed: tasks.filter((t) => t.status === "failed").length,
      running: 0,
      ready: 0,
      pending: 0,
      blocked: 0,
      skipped: 0,
      cancelled: 0,
      tokens: 0,
      cost: 0,
    },
  };
}

describe("reviewCircuitView & taskRequiresReview", () => {
  test("returns null when absent or normal with 0 rounds, and formats all 5 stages in en and zh", () => {
    setLang("en");
    const plain = makeTask();
    expect(taskRequiresReview(plain)).toBe(false);
    expect(reviewCircuitView(plain, translate)).toBeNull();

    const zeroNormal = makeTask({
      reviewCircuit: {
        stage: "normal",
        reviewRounds: 0,
        maxNormalRounds: 2,
        handoffToCoordinator: false,
        postHandoffReviewUsed: false,
        awaitingUserDecision: false,
        userOverrides: 0,
      },
    });
    expect(taskRequiresReview(zeroNormal)).toBe(true);
    expect(reviewCircuitView(zeroNormal, translate)).toBeNull();

    const r1 = makeTask({
      reviewCircuit: {
        stage: "normal",
        reviewRounds: 1,
        maxNormalRounds: 2,
        handoffToCoordinator: false,
        postHandoffReviewUsed: false,
        awaitingUserDecision: false,
        userOverrides: 0,
      },
    });
    const v1 = reviewCircuitView(r1, translate)!;
    expect(v1.tone).toBe("normal");
    expect(v1.shortLabel).toBe("Review 1/2");

    const handoff: ReviewCircuit = {
      stage: "handoff_to_coordinator",
      reviewRounds: 2,
      maxNormalRounds: 2,
      handoffToCoordinator: true,
      postHandoffReviewUsed: false,
      awaitingUserDecision: false,
      userOverrides: 0,
      history: [
        { round: 1, verdict: "changes_requested", summary: "R1 bug", at: "2026-11-24T00:01:00Z" },
        { round: 2, verdict: "changes_requested", summary: "R2 race", at: "2026-11-24T00:02:00Z" },
      ],
    };
    const vHandoff = reviewCircuitView(makeTask({ reviewCircuit: handoff }), translate)!;
    expect(vHandoff.tone).toBe("handoff");
    expect(vHandoff.shortLabel).toBe("R2/2 · Coord Handoff");
    expect(vHandoff.history).toHaveLength(2);

    const vPost = reviewCircuitView(
      makeTask({
        reviewCircuit: {
          ...handoff,
          stage: "post_handoff_review",
          postHandoffReviewUsed: true,
        },
      }),
      translate,
    )!;
    expect(vPost.tone).toBe("post_handoff");
    expect(vPost.shortLabel).toBe("Final Review 1/1");

    const vAwaiting = reviewCircuitView(
      makeTask({
        reviewCircuit: {
          ...handoff,
          stage: "awaiting_user_decision",
          reviewRounds: 3,
          postHandoffReviewUsed: true,
          awaitingUserDecision: true,
          userOverrides: 1,
        },
      }),
      translate,
    )!;
    expect(vAwaiting.tone).toBe("awaiting");
    expect(vAwaiting.shortLabel).toBe("R3 · Awaiting User");
    expect(vAwaiting.overrides).toBe(1);

    setLang("zh");
    const vAwaitingZh = reviewCircuitView(
      makeTask({
        reviewCircuit: {
          ...handoff,
          stage: "awaiting_user_decision",
          reviewRounds: 3,
          postHandoffReviewUsed: true,
          awaitingUserDecision: true,
          userOverrides: 0,
        },
      }),
      translate,
    )!;
    expect(vAwaitingZh.shortLabel).toBe("R3 · 等待用户决策");
    expect(vAwaitingZh.label).toBe("硬熔断 · 等待用户决策");

    const vClearedZh = reviewCircuitView(
      makeTask({
        reviewCircuit: {
          ...handoff,
          stage: "cleared",
          reviewRounds: 3,
          postHandoffReviewUsed: true,
          awaitingUserDecision: false,
          userOverrides: 1,
        },
      }),
      translate,
    )!;
    expect(vClearedZh.tone).toBe("cleared");
    expect(vClearedZh.shortLabel).toBe("审查第 3 轮 · 审查已通过");
  });
});

describe("workbench attention filtering and coordinator attention items", () => {
  test("taskMatchesFilter('attention') includes non-failed tasks in handoff or awaiting_user_decision", () => {
    const tNormal = makeTask({ id: "t1", status: "succeeded" });
    const tHandoff = makeTask({
      id: "t2",
      status: "succeeded",
      reviewCircuit: {
        stage: "handoff_to_coordinator",
        reviewRounds: 2,
        maxNormalRounds: 2,
        handoffToCoordinator: true,
        postHandoffReviewUsed: false,
        awaitingUserDecision: false,
        userOverrides: 0,
      },
    });
    const tAwaiting = makeTask({
      id: "t3",
      status: "succeeded",
      reviewCircuit: {
        stage: "awaiting_user_decision",
        reviewRounds: 3,
        maxNormalRounds: 2,
        handoffToCoordinator: true,
        postHandoffReviewUsed: true,
        awaitingUserDecision: true,
        userOverrides: 0,
      },
    });
    const tPost = makeTask({
      id: "t4",
      status: "running",
      reviewCircuit: {
        stage: "post_handoff_review",
        reviewRounds: 2,
        maxNormalRounds: 2,
        handoffToCoordinator: true,
        postHandoffReviewUsed: true,
        awaitingUserDecision: false,
        userOverrides: 0,
      },
    });

    expect(taskMatchesFilter(tNormal, "attention")).toBe(false);
    expect(taskMatchesFilter(tHandoff, "attention")).toBe(true);
    expect(taskMatchesFilter(tAwaiting, "attention")).toBe(true);
    expect(taskMatchesFilter(tPost, "attention")).toBe(false);

    const counts = countTasksByFilter([tNormal, tHandoff, tAwaiting, tPost]);
    expect(counts.attention).toBe(2);
    expect(counts.running).toBe(1);
    expect(counts.done).toBe(3);
  });

  test("buildCoordinatorAttention surfaces failed and non-failed circuit trips with accurate severity and ranking", () => {
    setLang("en");
    const tasks = [
      makeTask({
        id: "t_fail_hard",
        title: "Alpha Hard Failed",
        status: "failed",
        reviewCircuit: {
          stage: "awaiting_user_decision",
          reviewRounds: 3,
          maxNormalRounds: 2,
          handoffToCoordinator: true,
          postHandoffReviewUsed: true,
          awaitingUserDecision: true,
          userOverrides: 0,
        },
      }),
      makeTask({
        id: "t_fail_handoff",
        title: "Beta Handoff Failed",
        status: "failed",
        reviewCircuit: {
          stage: "handoff_to_coordinator",
          reviewRounds: 2,
          maxNormalRounds: 2,
          handoffToCoordinator: true,
          postHandoffReviewUsed: false,
          awaitingUserDecision: false,
          userOverrides: 0,
        },
      }),
      makeTask({
        id: "t_ok_hard",
        title: "Gamma Hard NonFailed",
        status: "succeeded",
        reviewCircuit: {
          stage: "awaiting_user_decision",
          reviewRounds: 3,
          maxNormalRounds: 2,
          handoffToCoordinator: true,
          postHandoffReviewUsed: true,
          awaitingUserDecision: true,
          userOverrides: 0,
        },
      }),
      makeTask({
        id: "t_ok_handoff",
        title: "Delta Handoff NonFailed",
        status: "succeeded",
        reviewCircuit: {
          stage: "handoff_to_coordinator",
          reviewRounds: 2,
          maxNormalRounds: 2,
          handoffToCoordinator: true,
          postHandoffReviewUsed: false,
          awaitingUserDecision: false,
          userOverrides: 0,
        },
      }),
    ];

    const items = buildCoordinatorAttention(makeSnapshot(tasks), translate);
    expect(items).toHaveLength(4);

    // `kind: "failed"` items rank ahead of `kind: "stall"`.
    expect(items.slice(0, 3).map((i) => i.kind)).toEqual(["failed", "failed", "failed"]);
    expect(items[3].kind).toBe("stall");

    const byId = new Map(items.map((i) => [i.id, i]));
    expect(byId.get("failed:t_fail_hard")).toMatchObject({
      severity: "danger",
      title: "Awaiting User",
      messageType: "failed",
    });
    expect(byId.get("failed:t_fail_handoff")).toMatchObject({
      severity: "danger",
      title: "Coord Handoff",
      messageType: "failed",
    });
    expect(byId.get("circuit:t_ok_hard")).toMatchObject({
      kind: "failed",
      severity: "danger",
      title: "Awaiting User",
      messageType: "escalation",
    });
    expect(byId.get("circuit:t_ok_handoff")).toMatchObject({
      kind: "stall",
      severity: "warn",
      title: "Coord Handoff",
      messageType: "escalation",
    });
  });
});
