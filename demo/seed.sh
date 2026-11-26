#!/bin/sh
# Optional demo content on laptop-a, written as the user through the admin
# page API: #shop's default recipient (lead), a weekday standup schedule in
# cron, and the opening channel post that starts the team conversation.
# Run after pair.sh, so the lead can reach frontend on laptop-b.
set -eu
cd "$(dirname "$0")"

page() { docker compose -f compose.yml exec -T -u dev laptop-a /demo/bin/page "$1"; }

echo "Default recipient of #shop: lead"
page '{"method":"channel_default","project":"shop","name":"lead"}' >/dev/null
echo "Schedule: weekdays 09:00, standup in #shop"
page '{"method":"save_schedule","id":null,"cron":"0 9 * * 1-5","project":"shop","to":null,"message":"Standup: post the checkout v2 status"}' >/dev/null
echo "Roles: release-lead for lead, qa for reviewer"
page '{"method":"save_role","name":"release-lead","prompt":"You lead the checkout v2 release. Keep a short plan, ask the people you need, and post a summary in #shop when everyone has answered.","previous":null}' >/dev/null
page '{"method":"save_role","name":"qa","prompt":"You check every change before release. Reply with blockers first, then a checklist.","previous":null}' >/dev/null
id_of() {
    docker compose -f compose.yml exec -T -u dev laptop-a /demo/bin/sbctl '{"method":"list","sender":null,"project":"shop"}' \
        | python3 -c "import json,sys; print(next(s['session_id'] for s in json.load(sys.stdin)['sessions'] if s['name'] == sys.argv[1]))" "$1"
}
page "{\"method\":\"assign_role\",\"session_id\":\"$(id_of lead)\",\"role\":\"release-lead\"}" >/dev/null
page "{\"method\":\"assign_role\",\"session_id\":\"$(id_of reviewer)\",\"role\":\"qa\"}" >/dev/null
echo "Opening post in #shop"
page '{"method":"channel_send","project":"shop","message":"@lead please plan the checkout v2 release"}' >/dev/null
echo "Done; the agents answer over the next half minute."
