$ErrorActionPreference = 'Stop'
$owner = $env:BABEL_STARTUP_OWNER
$sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$name = "BabelAudio-$sid"
$runKey = 'Software\Microsoft\Windows\CurrentVersion\Run'
$runValue = 'BabelAudio'

function Get-LegacyEntry($key) {
    if ($null -eq $key -or $key.GetValueNames() -notcontains $runValue) { return }
    if ($key.GetValueKind($runValue) -ne [Microsoft.Win32.RegistryValueKind]::String) {
        throw [System.IO.InvalidDataException]::new()
    }
    $key.GetValue($runValue, $null, [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
}

function Remove-LegacyEntry {
    if ($null -eq $legacy) { return }
    if ((Get-LegacyEntry $legacyKey) -cne $legacy) {
        throw [System.IO.IOException]::new('The login entry changed during the operation.')
    }
    $legacyKey.DeleteValue($runValue)
}

# Serialize Babel CLI/dashboard changes for this account across processes.
$mutex = [System.Threading.Mutex]::new($false, "Global\$name")
$locked = $false
$legacyKey = $null
try {
    try { $locked = $mutex.WaitOne(3000) }
    catch [System.Threading.AbandonedMutexException] { $locked = $true }
    if (!$locked) {
        [Console]::WriteLine('busy')
        exit 1
    }

    $operation = $env:BABEL_STARTUP_OPERATION
    $registry = [Microsoft.Win32.Registry]::CurrentUser
    $legacyKey = $registry.OpenSubKey($runKey)
    $legacy = Get-LegacyEntry $legacyKey
    if ($null -ne $legacy -and (
        !$legacy.StartsWith('"', [StringComparison]::Ordinal) -or
        !$legacy.Contains('babel-tray.exe" "--config"') -or
        !$legacy.EndsWith(('"--autostart-owner" "' + $owner + '"'), [StringComparison]::Ordinal)
    )) {
        [Console]::WriteLine('foreign-run')
        exit 1
    }

    $scheduler = New-Object -ComObject 'Schedule.Service'
    $scheduler.Connect()
    $folder = $scheduler.GetFolder('\')
    $task = $null
    try { $task = $folder.GetTask($name) }
    catch {
        # HRESULT_FROM_WIN32(ERROR_FILE_NOT_FOUND)
        if ($_.Exception.GetBaseException().HResult -ne -2147024894) { throw }
    }
    if ($null -ne $task -and $task.Definition.RegistrationInfo.Source -cne $owner) {
        [Console]::WriteLine('foreign-task')
        exit 1
    }

    if ($operation -eq 'status') {
        if ($null -ne $legacy) { [Console]::WriteLine("legacy \$name") }
        elseif ($null -ne $task -and $task.Enabled) { [Console]::WriteLine("enabled \$name") }
        else { [Console]::WriteLine("disabled \$name") }
        exit 0
    }

    if ($null -ne $legacy) {
        $legacyKey.Dispose()
        $legacyKey = $registry.OpenSubKey($runKey, $true)
    }

    if ($operation -eq 'enable') {
        $definition = $scheduler.NewTask(0)
        $definition.RegistrationInfo.Source = $owner
        $definition.RegistrationInfo.Description = 'Start the Babel tray and local dashboard at sign-in.'
        $definition.Principal.UserId = $sid
        $definition.Principal.LogonType = 3 # TASK_LOGON_INTERACTIVE_TOKEN
        $definition.Principal.RunLevel = 0 # TASK_RUNLEVEL_LUA
        $definition.Settings.ExecutionTimeLimit = 'PT0S'
        $definition.Settings.DisallowStartIfOnBatteries = $false
        $definition.Settings.StopIfGoingOnBatteries = $false
        $definition.Settings.MultipleInstances = 2 # TASK_INSTANCES_IGNORE_NEW

        $trigger = $definition.Triggers.Create(9) # TASK_TRIGGER_LOGON
        $trigger.UserId = $sid
        $trigger.Enabled = $true
        $trigger.Delay = 'PT10S' # Give the desktop shell time to initialize the tray.
        $action = $definition.Actions.Create(0) # TASK_ACTION_EXEC
        $action.Path = $env:BABEL_STARTUP_EXECUTABLE
        $action.Arguments = $env:BABEL_STARTUP_ARGUMENTS
        $action.WorkingDirectory = $env:BABEL_STARTUP_WORKING_DIR

        # TASK_CREATE or TASK_UPDATE, never overwrite a task that appeared after inspection.
        $flags = if ($null -eq $task) { 2 } else { 4 }
        $null = $folder.RegisterTaskDefinition($name, $definition, $flags, $sid, $null, 3, $null)
        try {
            Remove-LegacyEntry
        } catch {
            if ($null -eq $task) { $folder.DeleteTask($name, 0) }
            throw
        }
        [Console]::WriteLine("enabled \$name")
    } elseif ($operation -eq 'disable') {
        Remove-LegacyEntry
        try {
            if ($null -ne $task) { $folder.DeleteTask($name, 0) }
        } catch {
            if ($null -ne $legacy -and $null -eq (Get-LegacyEntry $legacyKey)) {
                $legacyKey.SetValue($runValue, $legacy, [Microsoft.Win32.RegistryValueKind]::String)
            }
            throw
        }
        [Console]::WriteLine("disabled \$name")
    }
} catch [System.IO.InvalidDataException] {
    [Console]::WriteLine('foreign-run')
    exit 1
} catch {
    # COM exceptions can contain action paths. Return only the numeric code.
    [Console]::WriteLine("error " + $_.Exception.GetBaseException().HResult)
    exit 1
} finally {
    if ($null -ne $legacyKey) { $legacyKey.Dispose() }
    if ($locked) { $mutex.ReleaseMutex() }
    $mutex.Dispose()
}
