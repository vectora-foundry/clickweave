/**
 * Tests for `RunWithValuesForm` (Task 1.J.4).
 *
 * Acceptance:
 * (a) all-defaulted skill skips the form (onSubmit called immediately)
 * (b) missing-required skill shows the form
 * (c) form submit invokes onSubmit with the right variables
 * (d) cancel calls onCancel
 */

import { render, screen, fireEvent } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup } from "@testing-library/react";
import { RunWithValuesForm } from "./RunWithValuesForm";
import type { Skill } from "../../bindings";

function makeSkill(variables: Skill["variables"]): Skill {
  return {
    id: "skl_test",
    version: 1,
    name: "Test Skill",
    description: "",
    state: "confirmed",
    scope: "project_local",
    tags: [],
    subgoal_text: "",
    subgoal_signature: "",
    applicability: { apps: [], hosts: [], signature: "" },
    parameter_schema: [],
    action_sketch: [],
    outputs: [],
    outcome_predicate: { type: "subgoal_completed", post_state_world_model_signature: null },
    provenance: [],
    stats: { occurrence_count: 1, success_rate: 1, last_seen_at: null, last_invoked_at: null },
    edited_by_user: false,
    created_at: "2026-01-01T00:00:00Z",
    updated_at: "2026-01-01T00:00:00Z",
    produced_node_ids: [],
    body: "",
    schema_version: 1,
    variables,
    sections: [],
  };
}

afterEach(cleanup);

describe("RunWithValuesForm — 1.J.4", () => {
  // (a) all-defaulted skill: no required vars → onSubmit called immediately with empty object
  it("(a) calls onSubmit immediately when skill has no variables", () => {
    const onSubmit = vi.fn();
    const onCancel = vi.fn();
    render(
      <RunWithValuesForm
        skill={makeSkill([])}
        onSubmit={onSubmit}
        onCancel={onCancel}
      />,
    );
    expect(onSubmit).toHaveBeenCalledWith({});
    expect(onCancel).not.toHaveBeenCalled();
  });

  // (b) missing required var → shows the form with the field
  it("(b) shows the form when there is a required variable (default=null)", () => {
    const onSubmit = vi.fn();
    render(
      <RunWithValuesForm
        skill={makeSkill([{ name: "recipient", type: "string", description: "Email address", default: null }])}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByTestId("run-with-values-form")).toBeInTheDocument();
    expect(screen.getByTestId("var-input-recipient")).toBeInTheDocument();
  });

  // (c) form submit with filled values invokes onSubmit with the variables
  it("(c) submits the form with the entered variable values", () => {
    const onSubmit = vi.fn();
    render(
      <RunWithValuesForm
        skill={makeSkill([
          { name: "recipient", type: "string", description: "Email address", default: null },
          { name: "subject", type: "string", description: null, default: "Hello" },
        ])}
        onSubmit={onSubmit}
        onCancel={vi.fn()}
      />,
    );
    // Fill required field
    fireEvent.change(screen.getByTestId("var-input-recipient"), {
      target: { value: "alice@example.com" },
    });
    // Submit button should be enabled
    const submitBtn = screen.getByTestId("run-with-values-submit");
    expect(submitBtn).not.toBeDisabled();
    fireEvent.click(submitBtn);
    expect(onSubmit).toHaveBeenCalledWith(
      expect.objectContaining({ recipient: "alice@example.com" }),
    );
  });

  it("(c) submit button is disabled when required fields are empty", () => {
    render(
      <RunWithValuesForm
        skill={makeSkill([{ name: "to", type: "string", description: null, default: null }])}
        onSubmit={vi.fn()}
        onCancel={vi.fn()}
      />,
    );
    expect(screen.getByTestId("run-with-values-submit")).toBeDisabled();
  });

  // (d) cancel calls onCancel
  it("(d) cancel button calls onCancel", () => {
    const onCancel = vi.fn();
    render(
      <RunWithValuesForm
        skill={makeSkill([{ name: "to", type: "string", description: null, default: null }])}
        onSubmit={vi.fn()}
        onCancel={onCancel}
      />,
    );
    fireEvent.click(screen.getByRole("button", { name: /cancel/i }));
    expect(onCancel).toHaveBeenCalled();
  });
});
