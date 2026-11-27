import type { Scenario } from "../framework/context";
import { projectsSessionExit } from "./projects-session-exit";
import { teamKickoff } from "./team-kickoff";

export const scenarios: Scenario[] = [teamKickoff, projectsSessionExit];
