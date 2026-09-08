param (
    [Parameter(Mandatory=$true, Position=0)]
    [string]$ServerHost,

    [Parameter(Position=1)]
    [string]$User = "root",

    [Parameter(Position=2)]
    [string]$RemoteDir = "~/maischedule",

    [switch]$ForceEnv
)

$ErrorActionPreference = "Stop"

# Auto-detect project root directory
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
if (Test-Path (Join-Path $ScriptDir "..\Dockerfile")) {
    $ProjectRoot = (Resolve-Path (Join-Path $ScriptDir "..")).Path
} else {
    $ProjectRoot = (Resolve-Path ".").Path
}
Set-Location $ProjectRoot

Write-Host "=== Deploying maischedule to $User@$ServerHost ($RemoteDir) ==="
Write-Host "Project root: $ProjectRoot"

$remoteTarget = "$User@$ServerHost"

# Step 1: Create remote directory
Write-Host "[1/6] Preparing remote directory and data storage..."
ssh $remoteTarget "mkdir -p $RemoteDir/data $RemoteDir/backups && chmod 777 $RemoteDir/data"

# Step 2: Build Docker image
Write-Host "[2/6] Building Docker image (linux/amd64)..."
docker build --platform linux/amd64 -t maischedule:latest .
if ($LASTEXITCODE -ne 0) {
    throw "Docker build failed"
}

# Step 3: Export image
Write-Host "[3/6] Exporting image to maischedule.tar..."
docker save -o maischedule.tar maischedule:latest
if ($LASTEXITCODE -ne 0) {
    throw "Docker save failed"
}

try {
    # Step 4: Copy archive and compose file with SSH on-the-fly compression
    Write-Host "[4/6] Copying docker-compose.yml and image to server (compressed transfer)..."
    scp -C maischedule.tar docker-compose.yml "$($remoteTarget):$($RemoteDir)/"
    if ($LASTEXITCODE -ne 0) {
        throw "File upload failed"
    }

    # Check remote .env
    Write-Host "Checking .env on server..."
    $checkEnvCmd = "test -f $RemoteDir/.env && echo exists || echo missing"
    $envStatus = ssh $remoteTarget $checkEnvCmd
    if ($envStatus -notmatch "exists" -or $ForceEnv) {
        if (Test-Path ".env") {
            Write-Host "Uploading local .env to server..."
            scp .env "$($remoteTarget):$($RemoteDir)/.env"
        } else {
            Write-Host "Notice: local .env not found. Please configure .env on the server."
        }
    } else {
        Write-Host "Remote .env already exists. Preserving remote configuration."
    }

    # Step 5: Backup DB, load image and start container
    Write-Host "[5/6] Backing up database, loading image and starting container on server..."
    $deployCmd = "cd $RemoteDir && if [ -f data/maischedule.db ]; then cp data/maischedule.db backups/maischedule_\$(date +%Y%m%d_%H%M%S).db && (ls -t backups/*.db 2>/dev/null | tail -n +6 | xargs -r rm -f --); fi && docker load -i maischedule.tar && docker compose up -d && rm -f maischedule.tar maischedule.tar.gz && docker image prune -f"
    ssh $remoteTarget $deployCmd
    if ($LASTEXITCODE -ne 0) {
        throw "Remote deployment commands failed"
    }

} finally {
    # Step 6: Cleanup local archive
    Write-Host "[6/6] Cleaning up local archive..."
    if (Test-Path maischedule.tar) {
        Remove-Item -Force maischedule.tar
    }
    if (Test-Path maischedule.tar.gz) {
        Remove-Item -Force maischedule.tar.gz
    }
}

Write-Host "=== Deployment completed successfully! ==="
