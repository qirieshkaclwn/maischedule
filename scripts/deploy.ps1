param (
    [Parameter(Mandatory=$true, Position=0)]
    [string]$ServerHost,

    [Parameter(Position=1)]
    [string]$User = "root",

    [Parameter(Position=2)]
    [string]$RemoteDir = "~/maischedule",

    [switch]$ForceEnv,
    [switch]$KeepRemoteEnv
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
Write-Host "[1/6] Preparing remote directories and data storage..."
ssh $remoteTarget "mkdir -p $RemoteDir/data $RemoteDir/backups $RemoteDir/nginx/conf.d $RemoteDir/data/certbot/conf $RemoteDir/data/certbot/www $RemoteDir/data/certbot/logs && chmod 777 $RemoteDir/data"

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
    # Step 4: Copy archive, compose file, server deploy script and nginx directory
    Write-Host "[4/6] Copying deployment files to server..."
    scp -C maischedule.tar docker-compose.yml scripts/server_deploy.sh "$($remoteTarget):$($RemoteDir)/"
    if ($LASTEXITCODE -ne 0) {
        throw "File upload failed"
    }

    scp -r -C nginx "$($remoteTarget):$($RemoteDir)/"
    if ($LASTEXITCODE -ne 0) {
        throw "Nginx directory upload failed"
    }

    # Check remote .env
    Write-Host "Checking .env configuration..."
    if (Test-Path ".env") {
        $checkEnvCmd = "if [ -f $RemoteDir/.env ]; then sha256sum $RemoteDir/.env | cut -d ' ' -f 1; else echo missing; fi"
        $remoteHash = (ssh $remoteTarget $checkEnvCmd).Trim()

        if ($remoteHash -eq "missing") {
            Write-Host "Remote .env does not exist. Uploading local .env to server..."
            scp .env "$($remoteTarget):$($RemoteDir)/.env"
        } else {
            $localHash = (Get-FileHash ".env" -Algorithm SHA256).Hash.ToLower()
            if ($KeepRemoteEnv) {
                Write-Host "Flag -KeepRemoteEnv specified. Preserving remote configuration."
            } elseif ($ForceEnv -or ($localHash -ne $remoteHash)) {
                Write-Host "Local .env has changed. Creating remote backup and uploading updated .env..."
                $backupTime = Get-Date -Format "yyyyMMdd_HHmmss"
                ssh $remoteTarget "cp $RemoteDir/.env $RemoteDir/backups/.env_$backupTime.bak"
                scp .env "$($remoteTarget):$($RemoteDir)/.env"
                Write-Host "Updated .env uploaded successfully."
            } else {
                Write-Host "Remote .env is already up-to-date with local .env."
            }
        }
    } else {
        Write-Host "Notice: local .env not found. Please configure .env on the server."
    }

    # Step 5: Execute server deployment script (SSL certbot check, DB backup, service launch)
    Write-Host "[5/6] Executing server deployment (SSL Let's Encrypt check/issue, backup, service launch)..."
    ssh $remoteTarget "chmod +x $RemoteDir/server_deploy.sh && $RemoteDir/server_deploy.sh $RemoteDir"
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
