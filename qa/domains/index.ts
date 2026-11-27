import { chats } from "./chats";
import { cron } from "./cron";
import { peers } from "./peers";
import { phones } from "./phones";
import { projects } from "./projects";
import { roles } from "./roles";
import { run } from "./run";
import { sessions } from "./sessions";
import { usage } from "./usage";
import { waiting } from "./waiting";

export const domains = [chats, sessions, waiting, cron, roles, projects, run, peers, usage, phones];
