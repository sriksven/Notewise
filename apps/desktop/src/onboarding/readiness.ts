/**
 * Setup readiness, and the decisions derived from it.
 *
 * Deliberately pure — no React, no fetch. Which steps exist, which one to show, and whether
 * Finish enables are the only real branching in this feature, and they are worth testing
 * without mounting anything.
 */

export type PermissionState = "not_requested" | "granted" | "denied" | "unavailable";

export interface PermissionReadiness {
  status: PermissionState;
  required: boolean;
  /** Why it is unavailable, when it is. Shown to the user verbatim. */
  detail: string | null;
}

export interface StepReadiness {
  satisfied: boolean;
  required: boolean;
}

export interface PermissionsReadiness extends StepReadiness {
  microphone: PermissionReadiness;
  system_audio: PermissionReadiness;
}

export interface SetupReadiness {
  /** RFC 3339, or null while setup has never been finished. */
  completed_at: string | null;
  steps: {
    model: StepReadiness;
    backend: StepReadiness;
    permissions: PermissionsReadiness;
  };
}

export type StepId = "welcome" | "model" | "backend" | "permissions";

export interface Step {
  id: StepId;
  title: string;
  satisfied: boolean;
  required: boolean;
}

/** The steps in wizard order. */
export function stepsFor(readiness: SetupReadiness): Step[] {
  const { model, backend, permissions } = readiness.steps;

  return [
    // Welcome has nothing to satisfy. It is marked satisfied rather than excluded so it can
    // still be rendered and navigated to without ever holding up the Finish button.
    { id: "welcome", title: "Welcome", satisfied: true, required: false },
    { id: "model", title: "Model", satisfied: model.satisfied, required: model.required },
    { id: "backend", title: "Backend", satisfied: backend.satisfied, required: backend.required },
    {
      id: "permissions",
      title: "Permissions",
      satisfied: permissions.satisfied,
      required: permissions.required,
    },
  ];
}

/**
 * Where Next from Welcome should land.
 *
 * Falls back to the last step rather than to Welcome, so someone who has already satisfied
 * everything reaches the screen with the Finish button on it.
 */
export function firstUnsatisfied(readiness: SetupReadiness): StepId {
  const steps = stepsFor(readiness).filter((step) => step.id !== "welcome");
  const next = steps.find((step) => step.required && !step.satisfied);
  return next?.id ?? steps[steps.length - 1].id;
}

/** Whether every required step is satisfied. */
export function canFinish(readiness: SetupReadiness): boolean {
  return stepsFor(readiness).every((step) => !step.required || step.satisfied);
}

/** Required steps that are not satisfied, for the post-completion banner. */
export function regressions(readiness: SetupReadiness): Step[] {
  return stepsFor(readiness).filter((step) => step.required && !step.satisfied);
}
