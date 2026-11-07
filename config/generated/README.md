# config/generated/ — rendered Pkl output, checked in
#
# These JSON files are what `pkl eval` produces from the schemas in
# config/schemas/ and the data in config/policies/:
#
#   pkl eval ../policies/schwab-policies.pkl -f json > policies.json
#   pkl eval ../policies/allowlist.pkl       -f json > allowlist.json
#
# HAND-GENERATED PENDING PKL EVAL: the pkl CLI was not available on
# the machine that first built this repo, so these files were written
# by hand in exactly the shape `pkl eval -f json` produces (top-level
# object, property names as in the schemas, listings as JSON arrays,
# absent optional fields omitted). Do NOT edit them by hand after
# this — edit config/policies/ and re-render.
#
# The proxy's Docker build re-renders them at build time
# (docker/proxy/Dockerfile, Pkl builder stage) and fails the build
# if the rendered output differs from the checked-in files, so
# hand-generated drift is caught in CI / at image build time.
