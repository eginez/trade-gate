# config/generated/ — rendered Pkl output, checked in
#
# These JSON files are what `pkl eval` produces from the schemas in
# config/schemas/ and the data in config/policies/:
#
#   pkl eval ../policies/policies.pkl -f json > policies.json
#   pkl eval ../policies/allowlist.pkl       -f json > allowlist.json
#
# Rendered with Pkl 0.27.0 (2026-11-07). Before that they were
# hand-written in the shape `pkl eval` was assumed to produce — and
# they were wrong in two ways: the schemas declared the properties
# inside classes instead of on the module, so real `pkl eval`
# failed, and the hand-written JSON was compact single-line while
# real pkl renders pretty-printed JSON. Both were invisible until
# the Docker build first ran pkl, because the check skips when pkl
# is absent. Do NOT edit them by hand — edit config/policies/ and
# re-render.
#
# The proxy's Docker build re-renders them at build time
# (docker/proxy/Dockerfile, Pkl builder stage) and fails the build
# if the rendered output differs from the checked-in files, so
# drift is caught in CI / at image build time.
