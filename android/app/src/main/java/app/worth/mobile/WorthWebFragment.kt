package app.worth.mobile

import android.os.Bundle
import android.view.View
import dev.hotwire.core.turbo.errors.VisitError
import dev.hotwire.navigation.destinations.HotwireDestinationDeepLink
import dev.hotwire.navigation.fragments.HotwireWebFragment

@HotwireDestinationDeepLink(uri = "hotwire://fragment/worth")
class WorthWebFragment : HotwireWebFragment() {
    override fun onViewCreated(view: View, savedInstanceState: Bundle?) {
        super.onViewCreated(view, savedInstanceState)
        toolbarForNavigation()?.apply {
            inflateMenu(R.menu.navigation)
            setOnMenuItemClickListener { item ->
                when (item.itemId) {
                    R.id.refresh -> { refresh(true); true }
                    R.id.open_browser -> { BrowserLauncher.open(requireContext(), location); true }
                    else -> false
                }
            }
        }
    }

    override fun createErrorView(error: VisitError): View =
        layoutInflater.inflate(R.layout.connection_error, hotwireView, false).apply {
            findViewById<View>(R.id.retry).setOnClickListener { refresh(true) }
        }
}
