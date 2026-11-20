package de.girocrab.app.ui.screens

import android.os.Build
import androidx.biometric.BiometricManager
import androidx.biometric.BiometricPrompt
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.graphics.compositeOver
import de.girocrab.app.ui.theme.GiroColors
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.AccountBalance
import androidx.compose.material.icons.filled.CloudDownload
import androidx.compose.material.icons.filled.FileDownload
import androidx.compose.material.icons.filled.Fingerprint
import androidx.compose.material.icons.filled.Lock
import androidx.compose.material.icons.filled.Science
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.scale
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextAlign
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.fragment.app.FragmentActivity

// ---------------------------------------------------------------------------
// Biometric lock
// ---------------------------------------------------------------------------

private fun authenticators(): Int =
    if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
        BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL
    } else {
        BiometricManager.Authenticators.BIOMETRIC_STRONG
    }

private fun FragmentActivity.canAuthenticate(): Boolean =
    BiometricManager.from(this).canAuthenticate(authenticators()) == BiometricManager.BIOMETRIC_SUCCESS

private fun FragmentActivity.showBiometricPrompt(onSuccess: () -> Unit, onFailure: (String?) -> Unit) {
    val prompt = BiometricPrompt(
        this,
        ContextCompat.getMainExecutor(this),
        object : BiometricPrompt.AuthenticationCallback() {
            override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) = onSuccess()
            override fun onAuthenticationError(errorCode: Int, errString: CharSequence) = onFailure(errString.toString())
        },
    )
    val builder = BiometricPrompt.PromptInfo.Builder()
        .setTitle("GiroCrab entsperren")
        .setSubtitle("Bestätige deine Identität, um den Tresor zu öffnen")
        .setAllowedAuthenticators(authenticators())
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) builder.setNegativeButtonText("Abbrechen")
    prompt.authenticate(builder.build())
}

/**
 * Full-screen lock. [onUnlock] runs after a successful biometric/device-credential check.
 * During a lock preview from the demo ([previewMode]) a "Zurück zur Demo" button leaves it without auth.
 */
@Composable
fun BiometricLockScreen(
    previewMode: Boolean,
    onUnlock: () -> Unit,
    onBackToDemo: () -> Unit,
    modifier: Modifier = Modifier,
    autoPrompt: Boolean = true,
) {
    val activity = LocalContext.current as? FragmentActivity
    var message by remember { mutableStateOf<String?>(null) }
    val available = remember(activity) { activity?.canAuthenticate() ?: false }

    fun prompt() {
        val a = activity ?: return
        if (!available) {
            // No secure lock on the device: do not trap the user outside their own vault.
            onUnlock(); return
        }
        message = null
        a.showBiometricPrompt(onSuccess = onUnlock, onFailure = { message = it })
    }

    LaunchedEffect(autoPrompt, previewMode) { if (autoPrompt && !previewMode) prompt() }

    val pulse by rememberInfiniteTransition(label = "crab").animateFloat(
        initialValue = 0.94f, targetValue = 1.06f,
        animationSpec = infiniteRepeatable(tween(1400), RepeatMode.Reverse), label = "pulse",
    )

    Surface(modifier.fillMaxSize(), color = MaterialTheme.colorScheme.background) {
        Column(
            Modifier.fillMaxSize().padding(32.dp),
            horizontalAlignment = Alignment.CenterHorizontally,
            verticalArrangement = Arrangement.Center,
        ) {
            Box(
                Modifier.size(112.dp).scale(pulse)
                    .let { it },
                contentAlignment = Alignment.Center,
            ) {
                Surface(shape = CircleShape, color = MaterialTheme.colorScheme.primaryContainer, modifier = Modifier.fillMaxSize()) {}
                // Crab signet: lock glyph on brand circle (vector crab asset arrives with the icon set).
                Text("🦀", style = MaterialTheme.typography.displayMedium)
            }
            Spacer(Modifier.height(24.dp))
            Text("GiroCrab ist gesperrt", style = MaterialTheme.typography.headlineSmall, fontWeight = FontWeight.SemiBold)
            Spacer(Modifier.height(8.dp))
            Text(
                if (previewMode) "Vorschau des Sperrbildschirms – in der Demo bleiben alle Daten unberührt."
                else "Entsperre mit Fingerabdruck, Gesicht oder Geräte-PIN.",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
                textAlign = TextAlign.Center,
            )
            message?.let {
                Spacer(Modifier.height(12.dp))
                Text(it, color = MaterialTheme.colorScheme.error, style = MaterialTheme.typography.bodySmall, textAlign = TextAlign.Center)
            }
            Spacer(Modifier.height(28.dp))
            Button(onClick = { if (previewMode) message = "Vorschau: Entsperren ist in der Demo deaktiviert." else prompt() }) {
                Icon(Icons.Default.Fingerprint, null)
                Spacer(Modifier.width(8.dp))
                Text("Entsperren")
            }
            if (previewMode) {
                Spacer(Modifier.height(8.dp))
                TextButton(onClick = onBackToDemo) { Text("Zurück zur Demo") }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Demo banner (#783)
// ---------------------------------------------------------------------------

/**
 * Shown on top of the stream while the demo is active. The primary action never overwrites real data:
 * without real data it starts own-account setup, with real data it returns to the own vault.
 */
@Composable
fun DemoBanner(
    vaultMayHoldRealData: Boolean,
    onCreateOwnAccount: () -> Unit,
    onBackToOwnVault: () -> Unit,
    onTestLockScreen: () -> Unit,
    modifier: Modifier = Modifier,
) {
    // Swift DemoBanner: Theme.selection fill (accent 0.15), 1 px accent 0.18 border, accent icon, Theme.text title.
    val accent = MaterialTheme.colorScheme.primary
    Surface(
        modifier.fillMaxWidth().heightIn(min = 34.dp),
        color = accent.copy(alpha = GiroColors.SelectionAlpha).compositeOver(MaterialTheme.colorScheme.surface),
        contentColor = MaterialTheme.colorScheme.onSurface,
        shape = RoundedCornerShape(12.dp),
        border = androidx.compose.foundation.BorderStroke(1.dp, accent.copy(alpha = GiroColors.DemoBorderAlpha)),
    ) {
        Row(Modifier.padding(start = 12.dp, end = 4.dp, top = 4.dp, bottom = 4.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Default.Science, null, Modifier.size(16.dp), tint = accent)
            Spacer(Modifier.width(6.dp))
            Column(Modifier.weight(1f)) {
                Text("Demo-Daten", style = MaterialTheme.typography.labelMedium, fontWeight = FontWeight.SemiBold)
                // Swift DemoBanner.resetNote (#952): the demo snapshot is rebuilt on every start.
                Text(
                    "Änderungen werden beim Beenden zurückgesetzt.",
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
            val compactPadding = androidx.compose.foundation.layout.PaddingValues(horizontal = 8.dp, vertical = 0.dp)
            if (!vaultMayHoldRealData) {
                TextButton(onClick = onCreateOwnAccount, contentPadding = compactPadding, colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = accent)) {
                    Text("Konto anlegen", style = MaterialTheme.typography.labelMedium)
                }
            } else {
                TextButton(onClick = onBackToOwnVault, contentPadding = compactPadding, colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = accent)) {
                    Text("Eigener Tresor", style = MaterialTheme.typography.labelMedium)
                }
            }
            TextButton(onClick = onTestLockScreen, contentPadding = compactPadding, colors = androidx.compose.material3.ButtonDefaults.textButtonColors(contentColor = accent)) {
                Text("Sperren", style = MaterialTheme.typography.labelMedium)
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Empty vault / onboarding
// ---------------------------------------------------------------------------

@Composable
fun EmptyVaultCard(
    onAddBank: () -> Unit,
    onImport: () -> Unit,
    onRestore: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Card(
        modifier.fillMaxWidth().padding(16.dp),
        shape = RoundedCornerShape(20.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surfaceVariant),
    ) {
        Column(Modifier.padding(20.dp), verticalArrangement = Arrangement.spacedBy(14.dp)) {
            Text("Willkommen bei GiroCrab", style = MaterialTheme.typography.titleLarge, fontWeight = FontWeight.SemiBold)
            Text(
                "Deine Umsätze bleiben verschlüsselt auf diesem Gerät. In drei Schritten startest du:",
                style = MaterialTheme.typography.bodyMedium,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            SetupStep(1, "Bank hinzufügen", "Zugangsdaten werden nur lokal im Tresor gespeichert.")
            SetupStep(2, "Umsätze abrufen", "GiroCrab lädt Konten und Buchungen direkt von deiner Bank.")
            SetupStep(3, "Kategorisieren", "Regeln und Kategorien sortieren deine Ausgaben automatisch.")
            Spacer(Modifier.height(4.dp))
            ActionButton(Icons.Default.AccountBalance, "Bank suchen und hinzufügen …", primary = true, onClick = onAddBank)
            ActionButton(Icons.Default.FileDownload, "Vorhandene Daten importieren", primary = false, onClick = onImport)
            ActionButton(Icons.Default.CloudDownload, "Aus Cloud/Backup wiederherstellen", primary = false, onClick = onRestore)
        }
    }
}

@Composable
private fun SetupStep(number: Int, title: String, detail: String) {
    Row(verticalAlignment = Alignment.Top, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
        Surface(shape = CircleShape, color = MaterialTheme.colorScheme.primary, modifier = Modifier.size(28.dp)) {
            Box(contentAlignment = Alignment.Center) {
                Text("$number", color = MaterialTheme.colorScheme.onPrimary, style = MaterialTheme.typography.labelLarge)
            }
        }
        Column {
            Text(title, style = MaterialTheme.typography.titleSmall, fontWeight = FontWeight.SemiBold)
            Text(detail, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun ActionButton(icon: ImageVector, label: String, primary: Boolean, onClick: () -> Unit) {
    val content: @Composable () -> Unit = {
        Icon(icon, null, Modifier.size(18.dp))
        Spacer(Modifier.width(8.dp))
        Text(label)
    }
    if (primary) Button(onClick, Modifier.fillMaxWidth()) { content() }
    else OutlinedButton(onClick, Modifier.fillMaxWidth()) { content() }
}
