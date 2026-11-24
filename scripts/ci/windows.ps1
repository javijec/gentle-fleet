param([ValidateSet("quick","full","release")][string]$Profile = "full")
$ErrorActionPreference = "Stop"
python "$PSScriptRoot/local_ci.py" --profile $Profile
exit $LASTEXITCODE
