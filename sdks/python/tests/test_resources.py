import pytest
from unittest.mock import MagicMock, AsyncMock


class TransactionResource:
    """Transaction resource for API operations."""

    def __init__(self, client):
        self.client = client

    async def list(self, **kwargs):
        return await self.client.get('/transactions', **kwargs)

    async def get(self, transaction_id):
        return await self.client.get(f'/transactions/{transaction_id}')

    async def create(self, data):
        return await self.client.post('/transactions', json=data)

    async def update(self, transaction_id, data):
        return await self.client.put(f'/transactions/{transaction_id}', json=data)


class SettlementResource:
    """Settlement resource for API operations."""

    def __init__(self, client):
        self.client = client

    async def list(self, **kwargs):
        return await self.client.get('/settlements', **kwargs)

    async def get(self, settlement_id):
        return await self.client.get(f'/settlements/{settlement_id}')

    async def create(self, data):
        return await self.client.post('/settlements', json=data)

    async def update(self, settlement_id, data):
        return await self.client.put(f'/settlements/{settlement_id}', json=data)


class AdminLockResource:
    """Admin lock resource for API operations."""

    def __init__(self, client):
        self.client = client

    async def list(self, **kwargs):
        return await self.client.get('/admin/locks', **kwargs)

    async def get(self, lock_id):
        return await self.client.get(f'/admin/locks/{lock_id}')

    async def create(self, data):
        return await self.client.post('/admin/locks', json=data)

    async def update(self, lock_id, data):
        return await self.client.put(f'/admin/locks/{lock_id}', json=data)


class ReconciliationResource:
    """Reconciliation resource for API operations."""

    def __init__(self, client):
        self.client = client

    async def list(self, **kwargs):
        return await self.client.get('/reconciliation', **kwargs)

    async def get(self, recon_id):
        return await self.client.get(f'/reconciliation/{recon_id}')

    async def create(self, data):
        return await self.client.post('/reconciliation', json=data)

    async def update(self, recon_id, data):
        return await self.client.put(f'/reconciliation/{recon_id}', json=data)


class StatsResource:
    """Stats resource for API operations."""

    def __init__(self, client):
        self.client = client

    async def get_metrics(self):
        return await self.client.get('/stats/metrics')

    async def get_volume(self, period):
        return await self.client.get(f'/stats/volume/{period}')


class TestTransactionResource:
    """Test suite for Transaction resource."""

    @pytest.fixture
    def mock_client(self):
        return AsyncMock()

    @pytest.fixture
    def resource(self, mock_client):
        return TransactionResource(mock_client)

    @pytest.mark.asyncio
    async def test_list_transactions(self, resource, mock_client):
        mock_client.get.return_value = {
            'data': [{'id': 'txn-1'}, {'id': 'txn-2'}]
        }

        result = await resource.list()
        assert 'data' in result
        mock_client.get.assert_called_with('/transactions')

    @pytest.mark.asyncio
    async def test_get_transaction(self, resource, mock_client):
        mock_client.get.return_value = {'id': 'txn-123', 'status': 'pending'}

        result = await resource.get('txn-123')
        assert result['id'] == 'txn-123'
        mock_client.get.assert_called_with('/transactions/txn-123')

    @pytest.mark.asyncio
    async def test_create_transaction(self, resource, mock_client):
        payload = {'amount': 100.0, 'asset_code': 'USD'}
        mock_client.post.return_value = {
            'id': 'txn-new',
            'amount': 100.0,
            'asset_code': 'USD'
        }

        result = await resource.create(payload)
        assert result['id'] == 'txn-new'
        mock_client.post.assert_called_with('/transactions', json=payload)

    @pytest.mark.asyncio
    async def test_update_transaction(self, resource, mock_client):
        payload = {'status': 'settled'}
        mock_client.put.return_value = {
            'id': 'txn-123',
            'status': 'settled'
        }

        result = await resource.update('txn-123', payload)
        assert result['status'] == 'settled'
        mock_client.put.assert_called_with('/transactions/txn-123', json=payload)

    @pytest.mark.asyncio
    async def test_list_with_pagination(self, resource, mock_client):
        mock_client.get.return_value = {'data': [], 'has_more': False}

        await resource.list(limit=25, offset=0)
        mock_client.get.assert_called_with('/transactions', limit=25, offset=0)


class TestSettlementResource:
    """Test suite for Settlement resource."""

    @pytest.fixture
    def mock_client(self):
        return AsyncMock()

    @pytest.fixture
    def resource(self, mock_client):
        return SettlementResource(mock_client)

    @pytest.mark.asyncio
    async def test_list_settlements(self, resource, mock_client):
        mock_client.get.return_value = {
            'data': [{'id': 'sett-1'}, {'id': 'sett-2'}]
        }

        result = await resource.list()
        assert 'data' in result

    @pytest.mark.asyncio
    async def test_get_settlement(self, resource, mock_client):
        mock_client.get.return_value = {'id': 'sett-456', 'status': 'pending'}

        result = await resource.get('sett-456')
        assert result['id'] == 'sett-456'

    @pytest.mark.asyncio
    async def test_create_settlement(self, resource, mock_client):
        payload = {'tenant_id': 'tenant-1', 'amount': 5000.0}
        mock_client.post.return_value = {
            'id': 'sett-new',
            'amount': 5000.0
        }

        result = await resource.create(payload)
        assert result['id'] == 'sett-new'

    @pytest.mark.asyncio
    async def test_update_settlement(self, resource, mock_client):
        payload = {'status': 'completed'}
        mock_client.put.return_value = {
            'id': 'sett-456',
            'status': 'completed'
        }

        result = await resource.update('sett-456', payload)
        assert result['status'] == 'completed'


class TestAdminLockResource:
    """Test suite for Admin Lock resource."""

    @pytest.fixture
    def mock_client(self):
        return AsyncMock()

    @pytest.fixture
    def resource(self, mock_client):
        return AdminLockResource(mock_client)

    @pytest.mark.asyncio
    async def test_list_admin_locks(self, resource, mock_client):
        mock_client.get.return_value = {'data': []}

        result = await resource.list()
        assert 'data' in result

    @pytest.mark.asyncio
    async def test_get_admin_lock(self, resource, mock_client):
        mock_client.get.return_value = {'id': 'lock-789', 'status': 'active'}

        result = await resource.get('lock-789')
        assert result['id'] == 'lock-789'

    @pytest.mark.asyncio
    async def test_create_admin_lock(self, resource, mock_client):
        payload = {'tenant_id': 'tenant-1', 'reason': 'compliance_hold'}
        mock_client.post.return_value = {'id': 'lock-new', 'reason': 'compliance_hold'}

        result = await resource.create(payload)
        assert result['id'] == 'lock-new'

    @pytest.mark.asyncio
    async def test_update_admin_lock(self, resource, mock_client):
        payload = {'status': 'released'}
        mock_client.put.return_value = {'id': 'lock-789', 'status': 'released'}

        result = await resource.update('lock-789', payload)
        assert result['status'] == 'released'


class TestReconciliationResource:
    """Test suite for Reconciliation resource."""

    @pytest.fixture
    def mock_client(self):
        return AsyncMock()

    @pytest.fixture
    def resource(self, mock_client):
        return ReconciliationResource(mock_client)

    @pytest.mark.asyncio
    async def test_list_reconciliation(self, resource, mock_client):
        mock_client.get.return_value = {'data': []}

        result = await resource.list()
        assert 'data' in result

    @pytest.mark.asyncio
    async def test_get_reconciliation(self, resource, mock_client):
        mock_client.get.return_value = {'id': 'recon-321', 'status': 'pending'}

        result = await resource.get('recon-321')
        assert result['id'] == 'recon-321'

    @pytest.mark.asyncio
    async def test_create_reconciliation(self, resource, mock_client):
        payload = {'tenant_id': 'tenant-1', 'period': '2024-08-01'}
        mock_client.post.return_value = {'id': 'recon-new', 'period': '2024-08-01'}

        result = await resource.create(payload)
        assert result['id'] == 'recon-new'

    @pytest.mark.asyncio
    async def test_update_reconciliation(self, resource, mock_client):
        payload = {'status': 'verified'}
        mock_client.put.return_value = {'id': 'recon-321', 'status': 'verified'}

        result = await resource.update('recon-321', payload)
        assert result['status'] == 'verified'


class TestStatsResource:
    """Test suite for Stats resource."""

    @pytest.fixture
    def mock_client(self):
        return AsyncMock()

    @pytest.fixture
    def resource(self, mock_client):
        return StatsResource(mock_client)

    @pytest.mark.asyncio
    async def test_get_metrics(self, resource, mock_client):
        mock_client.get.return_value = {
            'transactions_total': 1000,
            'settlements_total': 500
        }

        result = await resource.get_metrics()
        assert result['transactions_total'] == 1000

    @pytest.mark.asyncio
    async def test_get_volume_daily(self, resource, mock_client):
        mock_client.get.return_value = {'period': 'daily', 'volume': 50000.0}

        result = await resource.get_volume('daily')
        assert result['period'] == 'daily'

    @pytest.mark.asyncio
    async def test_get_volume_monthly(self, resource, mock_client):
        mock_client.get.return_value = {'period': 'monthly', 'volume': 1500000.0}

        result = await resource.get_volume('monthly')
        assert result['volume'] == 1500000.0


class TestResourceParity:
    """Test suite for resource parity with Rust SDK."""

    def test_all_resources_have_crud_operations(self):
        """Verify all resources implement CRUD operations."""
        mock_client = AsyncMock()

        resources = [
            TransactionResource(mock_client),
            SettlementResource(mock_client),
            AdminLockResource(mock_client),
            ReconciliationResource(mock_client),
        ]

        for resource in resources:
            assert hasattr(resource, 'list')
            assert hasattr(resource, 'get')
            assert hasattr(resource, 'create')
            assert hasattr(resource, 'update')

    def test_stats_resource_has_metrics_operations(self):
        """Verify stats resource has metrics operations."""
        mock_client = AsyncMock()
        resource = StatsResource(mock_client)

        assert hasattr(resource, 'get_metrics')
        assert hasattr(resource, 'get_volume')

    @pytest.mark.asyncio
    async def test_resource_methods_return_awaitable(self):
        """Verify resource methods return awaitables."""
        mock_client = AsyncMock()
        mock_client.get.return_value = {'id': 'test'}
        mock_client.post.return_value = {'id': 'test'}

        transaction = TransactionResource(mock_client)

        list_result = transaction.list()
        get_result = transaction.get('123')
        create_result = transaction.create({'amount': 100})

        assert hasattr(list_result, '__await__')
        assert hasattr(get_result, '__await__')
        assert hasattr(create_result, '__await__')
